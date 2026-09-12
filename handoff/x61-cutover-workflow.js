export const meta = { name: 'x61_arch_cutover', description: 'Finish the X61 architecture cutover in tiny reviewed increments with architecture-drift checks after every implementation step.', phases: [{ title: 'Inventory' }, { title: 'Increment Loop' }, { title: 'Final Review' }] }

const increments = [
  {
    name: 'gm965_config_pod_static',
    goal: 'Replace the mutable Gm965Ich8Board data path with const-buildable POD Gm965Ich8Config and an X61 static config, without changing board behavior.',
    scope: [
      'crates/fstart-platform-intel-gm965-ich8/src/lib.rs',
      'boards/lenovo-x61/src/config.rs',
      'boards/lenovo-x61/src/stage.rs'
    ],
    forbidden: ['driver objects in config', 'closures in config', 'trait objects in config', 'generic graph expansion']
  },
  {
    name: 'intel_fixed_early_flow',
    goal: 'Replace Gm965Ich8UefiRecipe/StageFlow for bootblock with a fixed Intel early flow and X61 hooks. Delete the old bootblock path in the same increment.',
    scope: [
      'crates/fstart-platform-intel-gm965-ich8/src/recipe.rs',
      'crates/fstart-stage-runtime/src/fixed_flow.rs',
      'crates/stage/src/lib.rs',
      'boards/lenovo-x61/src/stage.rs'
    ],
    forbidden: ['parallel recipe and fixed-flow models', 'HardwareInit in early flow', 'ordering DSL', 'payload-flavored flow names']
  },
  {
    name: 'mainstage_phases_payload_agnostic',
    goal: 'Move ramstage to the phase-oriented mainstage shape and remove payload-flavored recipe naming. Boot mode and payload remain build inputs, not board identity.',
    scope: [
      'crates/fstart-platform-intel-gm965-ich8/src/recipe.rs',
      'crates/stage/src/lib.rs',
      'boards/lenovo-x61/Cargo.toml',
      'xtask/src/build_plan.rs',
      'xtask/src/build_board.rs'
    ],
    forbidden: ['Gm965Ich8UefiRecipe', 'stage-recipe = "gm965-ich8-uefi"', 'payload choice in board identity']
  },
  {
    name: 'smm_selected_board_wrapper',
    goal: 'Route SMM through the selected-board wrapper so X61 binds LenovoX61SmmHandler. Delete FSTART_SMM_PLATFORM/cfg board registry selection.',
    scope: [
      'crates/fstart-smm-stage',
      'crates/fstart-smm-image',
      'boards/lenovo-x61/src/smm.rs',
      'boards/lenovo-x61/src/config.rs',
      'xtask/src/build_board.rs'
    ],
    forbidden: ['FSTART_SMM_PLATFORM', 'cfg(smm_platform)', 'NoBoardSmmHandler for lenovo-x61']
  },
  {
    name: 'host_metadata_cut',
    goal: 'Delete BoardInfo/BuildInfo/fstart-codegen board-loader dependence from the build path. xtask/fbuild boundary is Cargo.toml metadata, wrapper workspace, linker emission, image assembly.',
    scope: [
      'crates/fstart-types',
      'crates/fstart-codegen',
      'xtask/src/board_tool.rs',
      'xtask/src/build_board.rs',
      'xtask/src/build_plan.rs',
      'xtask/src/assemble.rs'
    ],
    forbidden: ['BuildInfo mega-builder', 'BoardInfo object model', 'central board loader', 'generated Rust stage code']
  },
  {
    name: 'crate_consolidation_x61_only',
    goal: 'Mechanically consolidate the X61 closure toward the target crate layout. Do not bring back attic boards or unported crates.',
    scope: [
      'Cargo.toml',
      'crates/'
    ],
    forbidden: ['attic board re-addition', 'new crate without architecture.md reason', 'behavior changes mixed with renames']
  }
]

function numberedList(items) {
  return items.map((item, index) => `${index + 1}. ${item}`).join('\n')
}

const previousFailure = [
  'Prior run failed because increment 1 added a large Gm965Ich8Config compatibility bridge instead of deleting an old seam.',
  'It duplicated X61 flash-layout facts: new const flash tables plus the existing x61_flash_layout() table.',
  'It moved board-attached LPC/SMBus devices into platform POD, violating docs/architecture.md: board-attached devices stay in board code/hooks.',
  'It added hundreds of lines while removing none of Gm965Ich8Board, Gm965Ich8UefiRecipe, StageFlow, HardwareInit, BoardInfo/BuildInfo, or SMM env/cfg selection.',
  'Do not repeat that pattern: each increment should delete/replace old-model code; if the smallest safe move is unclear, report that instead of adding a bridge.'
]

phase('Inventory')
const inventory = await agent(`Read docs/architecture.md and inspect the current repository state for the X61 cutover only.

Return a compact JSON status with:
- current blockers for the listed increments
- exact symbols/files that still violate the architecture
- smallest safe first increment
- how the next increment avoids this prior failed pattern:\n${numberedList(previousFailure)}

Do not edit files. Do not mention bringing back other boards.`, {
  label: 'inventory current state',
  tier: 'medium',
  schema: {
    type: 'object',
    additionalProperties: false,
    properties: {
      blockers: { type: 'array', items: { type: 'string' } },
      violations: { type: 'array', items: { type: 'string' } },
      firstIncrement: { type: 'string' }
    },
    required: ['blockers', 'violations', 'firstIncrement']
  }
})

phase('Increment Loop')
const results = []
for (let index = 0; index < increments.length; index++) {
  const item = increments[index]
  const previous = JSON.stringify(results)

  const implementation = await agent(`Implement exactly one tiny increment for the X61 cutover.

Increment ${index + 1}: ${item.name}
Goal: ${item.goal}
Scope paths:\n${numberedList(item.scope)}
Forbidden drift:\n${numberedList(item.forbidden)}

Read docs/architecture.md before editing. This increment must delete or replace at least one old-model seam unless the final diff is smaller than the starting diff. No compatibility bridge unless it is removed before this increment passes drift review. Do not bring back any attic board. Leave no commit.

Prior failed run to avoid:\n${numberedList(previousFailure)}

Previous increment results, if any:\n${previous}`, {
    label: `impl ${index + 1} ${item.name}`,
    tier: 'medium'
  })

  let drift = null
  let validation = null
  const fixes = []
  for (let attempt = 1; attempt <= 3; attempt++) {
    drift = await agent(`Architecture drift review for increment ${index + 1}: ${item.name}, attempt ${attempt}.

You must read docs/architecture.md and inspect the diff/worktree. Also compare against this prior failed run and fail if it repeats it:\n${numberedList(previousFailure)}

Fail on:
- net-new compatibility scaffolding where deletion/replacement was possible
- duplicated board facts or copied tables
- any parallel model left without being removed by this same increment
- config containing live drivers, closures, trait objects, runtime state, or board-attached device inventory
- recipe/StageFlow/HardwareInit/capability-event machinery surviving where this increment says it should die
- payload-flavored board identity
- SMM board registry by env/cfg
- fstart-codegen/BoardInfo/BuildInfo dependence after the host cut
- attic boards/crates being restored

Return JSON with pass, findings, requiredFixes, and whether the workflow should continue.`, {
      label: `drift ${index + 1}.${attempt} ${item.name}`,
      tier: 'big',
      schema: {
        type: 'object',
        additionalProperties: false,
        properties: {
          pass: { type: 'boolean' },
          findings: { type: 'array', items: { type: 'string' } },
          requiredFixes: { type: 'array', items: { type: 'string' } },
          continue: { type: 'boolean' }
        },
        required: ['pass', 'findings', 'requiredFixes', 'continue']
      }
    })

    if (drift.pass && drift.continue) {
      break
    }

    const fix = await agent(`Fix only the architecture drift from increment ${index + 1}: ${item.name}, attempt ${attempt}.

Required fixes:\n${numberedList(drift.requiredFixes)}

Rules:
- explicitly avoid the prior failed run:\n${numberedList(previousFailure)}
- prefer deleting the bad increment over adding more bridge code
- remove duplicated facts/tables
- keep board-attached devices in board code/hooks, not platform POD
- do not widen scope beyond this increment
- do not commit`, {
      label: `fix drift ${index + 1}.${attempt}`,
      tier: 'medium'
    })
    fixes.push(fix)
  }

  if (!drift.pass || !drift.continue) {
    results.push({ name: item.name, implementation, drift, validation, fixes })
    break
  }

  for (let attempt = 1; attempt <= 2; attempt++) {
    validation = await agent(`Run the smallest useful validation for increment ${index + 1}: ${item.name}, attempt ${attempt}.

Prefer, in order:
1. targeted cargo check/test for touched crates
2. cargo xtask build --board lenovo-x61 when build path changed
3. cargo fmt --check
4. rg checks proving forbidden symbols are gone

Return JSON with commands, pass, failures, and next minimal fix. Do not commit.`, {
      label: `validate ${index + 1}.${attempt} ${item.name}`,
      tier: 'medium',
      schema: {
        type: 'object',
        additionalProperties: false,
        properties: {
          pass: { type: 'boolean' },
          commands: { type: 'array', items: { type: 'string' } },
          failures: { type: 'array', items: { type: 'string' } },
          nextFix: { type: 'string' }
        },
        required: ['pass', 'commands', 'failures', 'nextFix']
      }
    })

    if (validation.pass) {
      break
    }

    const fix = await agent(`Fix only the validation failure for increment ${index + 1}: ${item.name}.

Failure:\n${validation.nextFix}\n${numberedList(validation.failures)}

Do not add compatibility scaffolding. Do not commit.`, {
      label: `fix validate ${index + 1}.${attempt}`,
      tier: 'medium'
    })
    fixes.push(fix)
  }

  results.push({ name: item.name, implementation, drift, validation, fixes })
  if (!validation.pass) {
    break
  }
}

phase('Final Review')
const finalReview = await agent(`Synthesize the X61 cutover workflow result for human review before any commit.

Use docs/architecture.md as the acceptance standard. Also state whether the workflow avoided this prior failed pattern:\n${numberedList(previousFailure)}

Summarize:
- increments completed
- files changed
- architecture drift findings
- validation commands/results
- exact remaining work
- whether it is safe for the human to review/commit

Do not claim other boards are restored.`, {
  label: 'final cutover review',
  tier: 'big',
  schema: {
    type: 'object',
    additionalProperties: false,
    properties: {
      okForHumanReview: { type: 'boolean' },
      completed: { type: 'array', items: { type: 'string' } },
      changedFiles: { type: 'array', items: { type: 'string' } },
      driftFindings: { type: 'array', items: { type: 'string' } },
      validations: { type: 'array', items: { type: 'string' } },
      remaining: { type: 'array', items: { type: 'string' } }
    },
    required: ['okForHumanReview', 'completed', 'changedFiles', 'driftFindings', 'validations', 'remaining']
  }
})

return { inventory, results, finalReview, commitReady: false }