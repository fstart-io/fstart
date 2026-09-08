#!/usr/bin/env python3
"""Opt-in real rust-analyzer acceptance probe for fbuild ide.

Requires the project's Rust toolchain, rust-src and rust-analyzer. Generates
views but never changes workspace/lock ownership. --edit-check briefly appends
an intentional compiler error to the selected board source, restoring it in a
finally block. Do not run that option alongside another writer to that file.
"""

import argparse
import json
import pathlib
import queue
import subprocess
import threading
import time
import tempfile

ROOT = pathlib.Path(__file__).resolve().parents[1]


def load_json(path):
    try:
        return json.loads(path.read_text())
    except (OSError, ValueError) as error:
        raise RuntimeError(f"cannot read generated JSON {path}: {error}") from error


def configuration(path):
    result = {}
    for key, value in load_json(path).items():
        parts = key.removeprefix("rust-analyzer.").split(".")
        at = result
        for part in parts[:-1]:
            at = at.setdefault(part, {})
        at[parts[-1]] = value
    return result


def position(text, word, last=False):
    offset = text.rindex(word) if last else text.index(word)
    before = text[:offset]
    return {"line": before.count("\n"), "character": len(before.rsplit("\n", 1)[-1]) + 1}


class Client:
    def __init__(self, config, directory):
        self.config = config
        self.events = queue.Queue()
        self.messages = []
        self.serial = 0
        self.versions = {}
        self.log = (directory / "rust-analyzer.log").open("w")
        self.process = subprocess.Popen(["rust-analyzer"], cwd=ROOT, stdin=subprocess.PIPE,
                                        stdout=subprocess.PIPE, stderr=self.log)
        assert self.process.stdin is not None and self.process.stdout is not None
        self.input = self.process.stdin
        self.output = self.process.stdout
        threading.Thread(target=self.read, daemon=True).start()

    def read(self):
        try:
            while True:
                headers = {}
                while True:
                    line = self.output.readline()
                    if not line:
                        return
                    if line == b"\r\n":
                        break
                    key, value = line.decode().split(":", 1)
                    headers[key.lower()] = value.strip()
                self.events.put(json.loads(self.output.read(int(headers["content-length"]))))
        finally:
            self.events.put({"eof": True})

    def send(self, value):
        data = json.dumps(value).encode()
        self.input.write(f"Content-Length: {len(data)}\r\n\r\n".encode() + data)
        self.input.flush()

    def notify(self, method, params):
        self.send({"jsonrpc": "2.0", "method": method, "params": params})

    def receive(self, deadline):
        value = self.events.get(timeout=max(0.01, deadline - time.monotonic()))
        self.messages.append(value)
        if value.get("eof"):
            raise RuntimeError("rust-analyzer exited")
        if "method" in value and "id" in value:
            result = None
            if value["method"] == "workspace/configuration":
                result = []
                for item in value["params"]["items"]:
                    section = (item.get("section") or "rust-analyzer").removeprefix("rust-analyzer").strip(".")
                    setting = self.config
                    for part in section.split(".") if section else []:
                        setting = setting.get(part, {})
                    result.append(setting)
            self.send({"jsonrpc": "2.0", "id": value["id"], "result": result})
        return value

    def request(self, method, params):
        # Cancellations can occur while a new project graph is being installed.
        for _ in range(3):
            self.serial += 1
            identifier = self.serial
            self.send({"jsonrpc": "2.0", "id": identifier, "method": method, "params": params})
            deadline = time.monotonic() + 180
            while True:
                value = self.receive(deadline)
                if value.get("id") == identifier and "method" not in value:
                    if value.get("error", {}).get("code") in (-32801, -32802):
                        break
                    if "error" in value:
                        raise RuntimeError(value)
                    return value.get("result")
        raise RuntimeError(f"repeated cancellation of {method}")

    def ready(self):
        deadline = time.monotonic() + 240
        expected = self.config["linkedProjects"][0]
        while True:
            value = self.receive(deadline)
            if value.get("method") == "experimental/serverStatus" and value["params"].get("quiescent"):
                assert value["params"]["health"] != "error", value
                status = self.request("rust-analyzer/analyzerStatus", {})
                if expected in status:
                    return status

    def initialize(self):
        self.request("initialize", {
            "processId": None, "rootUri": ROOT.as_uri(),
            "workspaceFolders": [{"uri": ROOT.as_uri(), "name": "fstart"}],
            "capabilities": {"workspace": {"configuration": True},
                             "window": {"workDoneProgress": True},
                             "experimental": {"serverStatusNotification": True}},
            "initializationOptions": self.config,
        })
        self.notify("initialized", {})
        return self.ready()

    def open(self, path):
        text, uri = path.read_text(), path.as_uri()
        if uri not in self.versions:
            self.versions[uri] = 1
            self.notify("textDocument/didOpen", {"textDocument": {
                "uri": uri, "languageId": "rust", "version": 1, "text": text}})
        return text, uri

    def change(self, uri, text):
        self.versions[uri] += 1
        self.notify("textDocument/didChange", {"textDocument": {"uri": uri, "version": self.versions[uri]},
                                               "contentChanges": [{"text": text}]})

    def at(self, method, uri, text, word, last=False):
        return self.request(method, {"textDocument": {"uri": uri}, "position": position(text, word, last)})

    def diagnostic(self, uri, present):
        deadline = time.monotonic() + 180
        while True:
            message = self.receive(deadline)
            if message.get("method") != "textDocument/publishDiagnostics" or message["params"]["uri"] != uri:
                continue
            found = any(d.get("code") == "E0308" for d in message["params"]["diagnostics"])
            if found == present:
                return

    def close(self, directory):
        (directory / "messages.json").write_text(json.dumps(self.messages, indent=2))
        self.process.terminate()
        try:
            self.process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.process.kill()
            self.process.wait()
        self.log.close()


def inspect(client, payload, release, board="riscv64"):
    architecture, trait = {"riscv64": ("riscv64", "QemuRiscv64VirtBoard"),
                           "armv7": ("arm", "QemuArmv7VirtBoard")}[board]
    platform = ROOT / f"crates/platform-qemu/src/virt_{board}.rs"
    stage = ROOT / f"boards/qemu/{board}/src/stage.rs"
    original, uri = client.open(stage)
    definitions = client.at("textDocument/definition", uri, original, trait)
    assert len(definitions) == 1 and definitions[0]["uri"] == platform.as_uri(), definitions
    selected = "crabefi" if payload == "uefi" else payload
    debug = "not(debug_assertions)" if release else "debug_assertions"
    condition = f'target_arch="{architecture}", fstart_entry="{board}", fstart_payload="{selected}", {debug}, not(test)'
    addition = (f"\n#[cfg(all({condition}))]\nconst FSTART_IDE_CFG: u8 = 1;\n"
                f"#[cfg(not(all({condition})))]\nconst FSTART_IDE_CFG: u8 = 2;\n"
                "fn fstart_ide_cfg_probe() -> u8 { FSTART_IDE_CFG }\n")
    try:
        client.change(uri, original + addition)
        hover = client.at("textDocument/hover", uri, original + addition, "FSTART_IDE_CFG", last=True)
        assert hover and "= 1" in hover["contents"]["value"], hover
    finally:
        client.change(uri, original)
    text, uri = client.open(platform)
    definitions = client.at("textDocument/definition", uri, text, "Selected as")
    expected = {"halt": "HaltPayload", "linux": "LinuxPayload", "uefi": "Riscv64UefiPayload"}[payload]
    assert len(definitions) == 1 and expected in text.splitlines()[definitions[0]["range"]["start"]["line"]], definitions
    text, uri = client.open(ROOT / "crates/core/src/board.rs")
    expansion = client.at("rust-analyzer/expandMacro", uri, text, "Serialize)]")
    assert expansion and "impl _serde::Serialize for BoardConfig" in expansion["expansion"], expansion
    assert client.request("workspace/symbol", {"query": "Gm965Ich8Config"}) == []
    print(f"PASS {board} {payload} {'release' if release else 'debug'}: real-source definition, cfgs, selected launcher, proc macro, bounded symbols", flush=True)


def edit_check(client):
    path = ROOT / "boards/qemu/riscv64/src/stage.rs"
    original, uri = client.open(path)
    modified = original + '\nconst FSTART_IDE_BAD: u32 = "deliberate compiler diagnostic";\n'
    try:
        assert path.read_text() == original, "concurrent board edit"
        path.write_text(modified)
        client.change(uri, modified)
        client.notify("textDocument/didSave", {"textDocument": {"uri": uri}})
        client.diagnostic(uri, True)
        subprocess.run(["cargo", "run", "--locked", "-p", "fbuild", "--", "ide", "qemu-riscv64",
                        "--release", "--payload", "halt"], cwd=ROOT, check=True)
        print("PASS editor generation with an existing source type error", flush=True)
    finally:
        current = path.read_text()
        if current == modified:
            path.write_text(original)
        elif current != original:
            raise RuntimeError("concurrent board edit; refusing to overwrite it")
        client.change(uri, original)
        client.notify("textDocument/didSave", {"textDocument": {"uri": uri}})
    client.diagnostic(uri, False)
    print("PASS compiler E0308 on original source URI, then cleared after restoration", flush=True)


def inventory_probe(count, evidence):
    """Grow discovery inventory, not the selected source/compilation graph."""
    view = ROOT / "target/fstart-ide/qemu-riscv64/release/halt"
    before = load_json(view / "rust-project.json")
    with tempfile.TemporaryDirectory(prefix="ide-noise-", dir=ROOT / "boards") as temporary:
        vendor = pathlib.Path(temporary)
        for index in range(count):
            board = vendor / str(index)
            (board / "src").mkdir(parents=True)
            name = f"{vendor.name}-{index}"
            (board / "Cargo.toml").write_text(
                f'[package]\nname="{name}"\nversion="0.0.0"\nedition="2024"\n'
                f'[package.metadata.fstart]\nschema=1\nboard="{name}"\n')
            (board / "src/lib.rs").write_text("#![no_std]\n")
        subprocess.run(["cargo", "run", "--locked", "-p", "fbuild", "--", "ide", "qemu-riscv64",
                        "--release", "--payload", "halt"], cwd=ROOT, check=True)
        assert load_json(view / "rust-project.json") == before, "unrelated inventory changed editor graph"
        directory = evidence / "inventory-noise"
        directory.mkdir(exist_ok=True)
        client = Client(configuration(view / "rust-analyzer.json"), directory)
        try:
            started = time.monotonic()
            client.initialize()
            inspect(client, "halt", True)
            elapsed = time.monotonic() - started
            (directory / "timing.json").write_text(json.dumps({"unrelated_boards": count,
                "startup_and_probes_seconds": elapsed, "compiler_units": len(before["crates"])}))
            print(f"PASS {count} unrelated boards: identical graph; LSP startup + probes {elapsed:.2f}s", flush=True)
        finally:
            client.close(directory)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--edit-check", action="store_true")
    parser.add_argument("--audit-lock", action="store_true")
    parser.add_argument("--inventory-noise", type=int, default=0)
    args = parser.parse_args()
    if args.inventory_noise < 0:
        parser.error("--inventory-noise must be nonnegative")
    cases = [("riscv64", "halt", True), ("riscv64", "linux", True),
             ("riscv64", "uefi", True), ("riscv64", "halt", False),
             ("armv7", "halt", True), ("armv7", "linux", True),
             ("riscv64", "halt", True)]
    paths = []
    for board, payload, release in cases:
        command = ["cargo", "run", "--locked", "-p", "fbuild", "--", "ide", f"qemu-{board}", "--payload", payload]
        if release:
            command.append("--release")
        if args.audit_lock:
            command.append("--audit-lock")
        subprocess.run(command, cwd=ROOT, check=True)
        directory = ROOT / f"target/fstart-ide/qemu-{board}" / ("release" if release else "debug") / payload
        project = load_json(directory / "rust-project.json")
        for crate in project["crates"]:
            source = pathlib.Path(crate["root_module"])
            assert source.resolve() == source, source
            if source.is_relative_to(ROOT / "boards"):
                assert source.is_relative_to(ROOT / f"boards/qemu/{board}"), source
        paths.append(directory / "rust-analyzer.json")
    evidence = ROOT / "target/fstart-ide/proof"
    evidence.mkdir(parents=True, exist_ok=True)
    client = Client(configuration(paths[0]), evidence)
    try:
        for index, ((board, payload, release), config_path) in enumerate(zip(cases, paths)):
            started = time.monotonic()
            if index == 0:
                status = client.initialize()
            else:
                client.config = configuration(config_path)
                client.notify("workspace/didChangeConfiguration", {"settings": None})
                status = client.ready()
            (evidence / f"{index}-status.txt").write_text(status)
            inspect(client, payload, release, board)
            print(f"  switch + semantic probes: {time.monotonic() - started:.2f}s", flush=True)
            if index == 0 and args.edit_check:
                edit_check(client)
    finally:
        client.close(evidence)
    if args.inventory_noise:
        inventory_probe(args.inventory_noise, evidence)


if __name__ == "__main__":
    main()
