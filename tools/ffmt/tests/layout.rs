// SPDX-License-Identifier: MIT OR Apache-2.0
use ffmt::{check_source, fix_source};

const PACKED: &str = r#"register_bitfields![u32,
    pub BRIDGE_BUSES [
        PRIMARY OFFSET(0) NUMBITS(8) [], SECONDARY OFFSET(8) NUMBITS(8) [],
        SUBORDINATE OFFSET(16) NUMBITS(8) []
    ],
    pub MODES [
        P2P OFFSET(1) NUMBITS(2) [Drop = 0, Forward = 1],
        OTHER OFFSET(5) NUMBITS(1) []
    ]
];
"#;

const FORMATTED: &str = r#"register_bitfields![u32,
    pub BRIDGE_BUSES [
        PRIMARY OFFSET(0) NUMBITS(8) [],
        SECONDARY OFFSET(8) NUMBITS(8) [],
        SUBORDINATE OFFSET(16) NUMBITS(8) []
    ],
    pub MODES [
        P2P OFFSET(1) NUMBITS(2) [
            Drop = 0,
            Forward = 1
        ],
        OTHER OFFSET(5) NUMBITS(1) []
    ]
];
"#;

fn fixed(src: &str) -> String {
    let out = fix_source(src).unwrap().unwrap();
    assert!(check_source(&out).unwrap().is_empty(), "{out}");
    assert_eq!(fix_source(&out).unwrap(), None, "not idempotent");
    // Supplemental invariant: the implementation itself only edits whitespace
    // gaps, so even whitespace *inside* comments and literals is preserved.
    let significant = |s: &str| s.chars().filter(|c| !c.is_whitespace()).collect::<String>();
    assert_eq!(significant(src), significant(&out));
    out
}

fn clean(src: &str) {
    assert!(check_source(src).unwrap().is_empty());
    assert_eq!(fix_source(src).unwrap(), None);
}

#[test]
fn reports_and_fixes_packed_lists() {
    let found = check_source(PACKED).unwrap();
    assert_eq!(found.len(), 2);
    assert_eq!(found[0].line, 3);
    assert!(found[0].message.contains("2 bitfields"));
    assert_eq!(found[1].line, 7);
    assert!(found[1].message.contains("2 enum variants"));
    assert_eq!(fixed(PACKED), FORMATTED);
    clean(FORMATTED);
}

#[test]
fn untouched_source_is_byte_identical() {
    let prefix = "// unrelated  spacing\nconst C: char = 'é';\nconst S: &str = \"\";\n";
    let suffix = "\n// trailing  whitespace  \nfn unrelated( ) {}\n";
    let out = fixed(&format!("{prefix}{PACKED}{suffix}"));
    assert_eq!(out, format!("{prefix}{FORMATTED}{suffix}"));
}

#[test]
fn single_items_and_already_separate_items_stay_untouched() {
    clean("register_bitfields![u32, R [A OFFSET(0) NUMBITS(1) [V = 0]]];");
    clean(
        "register_bitfields![u32, R [A OFFSET(0) NUMBITS(1) [],], S [B OFFSET(0) NUMBITS(1) []]];",
    );
    clean("register_bitfields![u32, R [A OFFSET(0) NUMBITS(1) [],\n B OFFSET(1) NUMBITS(1) []]];");
}

#[test]
fn invocation_spacing_and_delimiters() {
    for invocation in [
        "register_bitfields !",
        "register_bitfields /* hi */ !",
        "register_bitfields! /* hi */",
    ] {
        let src = format!(
            "{invocation} [u32, R [A OFFSET (0) NUMBITS (1) [], B OFFSET (1) NUMBITS (1) []]];"
        );
        let out = fixed(&src);
        assert!(out.contains("A OFFSET (0) NUMBITS (1) [],\n"));
    }
    fixed("register_bitfields! {u32, R [A OFFSET(0) NUMBITS(1) [], B OFFSET(1) NUMBITS(1) []]}");
}

#[test]
fn literals_comments_and_other_macro_names_are_not_invocations() {
    for src in [
        "//! register_bitfields![u32, R [A, B]]\n",
        "/* outer /* nested */ register_bitfields![u32, R [A, B]] */",
        "const S: &str = r###\"register_bitfields![u32, R [A, B]]\"###;",
        "éregister_bitfields![u32, R [A, B]];",
    ] {
        clean(src);
    }
    for prefix in [
        "const C: char = '\"';\n",
        "const S: &str = \"\";\n",
        "const C: char = 'é';\n",
        "\u{feff}",
        "#!/usr/bin/env rust-script\n",
        "\u{feff}#!/usr/bin/env rust-script\n",
    ] {
        let out = fixed(&format!("{prefix}{PACKED}"));
        assert!(out.starts_with(prefix));
    }
}

#[test]
fn expressions_are_preserved_verbatim() {
    for value in [
        "\"a  b [\".len() as u32",
        "r##\"a  b [\"##.len() as u32",
        "'[' as u32",
        "(1 == 1) as u32",
        "crate::value::<u8, u16>()",
        "crate::value::<Vec<u8>, u16>()",
        "a < b",
        "{ /* nested /* comment */ */ 0 }",
    ] {
        let src =
            format!("register_bitfields![u32, R [A OFFSET(0) NUMBITS(2) [V = {value}, W = 2]]];");
        let out = fixed(&src);
        assert!(out.contains(value), "{out}");
        clean(&format!(
            "register_bitfields![u32, R [A OFFSET(0) NUMBITS(2) [V = {value}]]];"
        ));
    }
}

#[test]
fn trailing_line_comments_keep_their_owner() {
    let src = "register_bitfields![u32, R [A OFFSET(0) NUMBITS(2) [Off = 0, On = 1, // On comment\n Extra = 2], B OFFSET(2) NUMBITS(1) [], // B comment\n C OFFSET(3) NUMBITS(1) []], // R comment\n S [X OFFSET(0) NUMBITS(1) []]];";
    let out = fixed(src);
    for text in [
        "On = 1, // On comment\n",
        "B OFFSET(2) NUMBITS(1) [], // B comment\n",
        "], // R comment\n",
    ] {
        assert!(out.contains(text), "{out}");
    }
}

#[test]
fn ambiguous_inline_comments_are_refused_without_writing() {
    for list in [
        "/* first? */ A OFFSET(0) NUMBITS(1) [], B OFFSET(1) NUMBITS(1) []",
        "A OFFSET(0) NUMBITS(1) [], /* whose? */ B OFFSET(1) NUMBITS(1) []",
        "A OFFSET(0) NUMBITS(1) [X = 0, /* whose? */ Y = 1]",
    ] {
        let src = format!("register_bitfields![u32, R [{list}]];");
        assert!(
            fix_source(&src)
                .unwrap_err()
                .contains("ambiguous inline comment")
        );
        assert!(check_source(&src).is_err());
    }
}

#[test]
fn comments_inside_items_and_at_end_are_untouched() {
    let src = "register_bitfields![u32, R [A /* note */ OFFSET(0) NUMBITS(1) [], B OFFSET(1) NUMBITS(1) [] /* last */]];";
    let out = fixed(src);
    assert!(out.contains("A /* note */ OFFSET(0) NUMBITS(1) []"));
    assert!(out.contains("B OFFSET(1) NUMBITS(1) [] /* last */\n"));
}

#[test]
fn documentation_and_attributes_keep_their_layout() {
    let src = "register_bitfields![u32,\n    // Banner  \n\n    /// Register.  \n    pub(crate) R [\n        /// Field A.  \n        #[doc = \"a\"] A OFFSET(0) NUMBITS(2) [X = 0, Y = 1], B OFFSET(2) NUMBITS(1) []\n    ]\n];";
    let out = fixed(src);
    for text in [
        "// Banner  \n\n    /// Register.  \n",
        "/// Field A.  \n        #[doc = \"a\"] A",
        "pub(crate) R [",
    ] {
        assert!(out.contains(text), "{out}");
    }
    // Documentation is source text, not a re-emitted #[doc] attribute.
    let src = "register_bitfields![u32, R [A OFFSET(0) NUMBITS(2) [/// First\n X = 0, Y = 1]]];";
    assert!(fixed(src).contains("/// First\n X = 0,"));
}

#[test]
fn trailing_commas_and_unaffected_registers_survive() {
    let src = "register_bitfields![u32, R [A OFFSET(0) NUMBITS(1) [],], S [B OFFSET(0) NUMBITS(1) [], C OFFSET(1) NUMBITS(1) [],]];";
    let out = fixed(src);
    assert!(out.contains("R [A OFFSET(0) NUMBITS(1) [],],"));
    assert!(out.contains("C OFFSET(1) NUMBITS(1) [],\n"));
}

#[test]
fn indentation_uses_the_parent_field_and_preserves_tabs() {
    for indent in ["    ", "\t"] {
        let src = format!(
            "fn f() {{\n{indent}register_bitfields![u32,\n{indent}    R [A OFFSET(0) NUMBITS(2) [X = 0, Y = 1], B OFFSET(2) NUMBITS(1) []]\n{indent}];\n}}"
        );
        let out = fixed(&src);
        assert!(
            out.contains(&format!(
                "\n{indent}        A OFFSET(0) NUMBITS(2) [\n{indent}            X = 0,"
            )),
            "{out}"
        );
    }
}

#[test]
fn nested_invocations_are_fixed_in_one_pass() {
    let src = "register_bitfields![u32, R [A OFFSET(0) NUMBITS(1) [V = { register_bitfields![u32, S [B OFFSET(0) NUMBITS(1) [], C OFFSET(1) NUMBITS(1) []]]; 0 }]]];";
    let out = fixed(src);
    assert!(out.contains("B OFFSET(0) NUMBITS(1) [],\n"));
}

#[test]
fn templates_are_skipped_but_concrete_macros_in_definitions_are_checked() {
    clean(
        "macro_rules! registers { ($name:ident) => { register_bitfields![u32, $name [A OFFSET(0) NUMBITS(1) [], B OFFSET(1) NUMBITS(1) []]]; } }",
    );
    fixed(
        "macro_rules! registers { () => { register_bitfields![u32, R [A OFFSET(0) NUMBITS(1) [], B OFFSET(1) NUMBITS(1) []]]; } }",
    );
}

#[test]
fn malformed_source_and_unrecognised_shapes_are_errors() {
    for src in [
        "register_bitfields![",
        "register_bitfields![u32, R];",
        "register_bitfields![u32, R [A]];",
        "register_bitfields![u32, R [A OFFSET(0) NUMBITS(1) [X]]];",
    ] {
        assert!(check_source(src).is_err(), "{src}");
        assert!(fix_source(src).is_err(), "{src}");
    }
}

#[test]
fn crlf_is_checked_but_not_rewritten() {
    let src = PACKED.replace('\n', "\r\n");
    assert_eq!(check_source(&src).unwrap().len(), 2);
    assert!(fix_source(&src).unwrap_err().contains("CRLF"));
}
