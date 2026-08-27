# Add the social preview to the README and bring it up to the current code

## Summary

Adds the Rebase social artwork to the top of `README.md`, hotlinked from the
NEAT-AI hub repo, and corrects the prose that had drifted from the code.
Closes #40.

The image is a hotlink, not a committed file — the same pattern
NEAT-AI-Forests uses, so a 1.2 MB PNG does not land in this repo:

```markdown
![NEAT-AI-Rebase](https://raw.githubusercontent.com/stSoftwareAU/NEAT-AI/Develop/docs/brand/social-previews/neat-ai-rebase.png)
```

The rest is a verification pass over the whole README against the workspace.
What no longer matched:

- the command-line synopsis predated `--harvest-from`, so it showed
  `--enhancements` as unconditionally required when clap declares it
  `required_unless_present = "harvest_from"`, and omitted both screening flags;
- `--output-dir` was described as taking "the three outputs" while the table
  below it lists four — `scoring/` is written too;
- `--screen-held-out` takes an explicit `true`/`false` (`ArgAction::Set`), not
  a bare flag;
- the examples paragraph covered two of the six in `rebase/examples/`;
- Status did not mention harvesting or sampled screening, which landed in
  `ec1afa4`.

Prose that already matched the code was left alone, and no section was added or
restructured. The Status section deliberately still describes producer wiring as
the pending next step: #8 owns that wording until Ockham integration lands.

## Evidence

No web interface to screenshot — this is a documentation change. The
Playwright MCP browser tools were **not registered in this session**
(`ToolSearch` for `browser_navigate` / `browser_take_screenshot` returned
"No matching deferred tools found"), and no browser binary was available to
substitute: no `chromium`/`google-chrome` on `PATH` and no
`~/.cache/ms-playwright`. Two bounded attempts to install one both failed —
`npx playwright install --with-deps chromium` exited 1 with
`Failed to install browsers / Error: Installation process exited with code: 1`
(it needs root for the apt step), and `npx playwright install chromium`
without `--with-deps` hit the 420 s timeout (exit 124) still downloading.

The image was verified directly instead:

| Check | Result |
| --- | --- |
| Hotlink resolves | `HTTP 200`, `content-type: image/png`, 1 187 543 bytes |
| File is a real PNG | magic bytes `\x89PNG`, `1280 x 640`, 8-bit RGBA |
| Dimensions | 1280×640 — GitHub's social-preview aspect |
| Pattern matches Forests | `NEAT-AI-Forests/README.md` line 3 is the same construct against `neat-ai-forests.png` |

Every corrected claim was checked against the source rather than assumed:

| README claim | Verified against |
| --- | --- |
| `--enhancements` required unless `--harvest-from` | `rebase/src/cli.rs:87-91` (`required_unless_present`) |
| `--harvest-from` mutually exclusive with `--enhancements` | `rebase/src/cli.rs:105` (`conflicts_with`) |
| `--screen-held-out` takes an explicit `true`/`false`, default `true` | `rebase/src/cli.rs:164` (`default_value_t = true`, `ArgAction::Set`) |
| `--min-improvement` `1e-9`, `--max-candidates` `8` | `rebase/src/cli.rs:129-134` |
| `scoring/` is a fourth output | `rebase/src/cli.rs:432` (`output_dir.join("scoring")`) |
| Exit codes `0` / `3` / `4` / `1` | `rebase/src/cli.rs:55-61` |
| Six examples, two fixture-only and four against real creatures | `rebase/examples/` and each file's header docs |
| Five documentation links | all five files present in `docs/` |

## Test Plan

No tests added: the change is documentation only, and the README claims it
corrects are already pinned by existing tests, which pass unchanged.

- `rebase/src/cli.rs::cli_parses_the_documented_invocation` — parses the exact
  invocation the README's Quick start prints, so a drifted flag name fails.
- `rebase/src/cli.rs::help_explains_that_the_champion_must_be_freshly_fetched`
  — guards the "fetch the champion immediately before running" warning.
- `rebase/src/cli.rs::screening_narrows_the_cohort_to_what_earns_its_place` and
  `a_screen_that_kills_everything_publishes_nothing` — the screening behaviour
  the flag table and Screening section describe.
- `rebase/src/cli.rs::harvest_from_derives_the_bundle_from_a_creature` and
  `harvest_from_a_creature_with_nothing_new_is_nothing_to_do` — the
  `--harvest-from` behaviour newly documented in the synopsis.

`./quality.sh` passes end to end: shellcheck, `cargo fmt --check`, clippy with
`-D warnings`, the full workspace test run (15 race-condition tests, the CLI
suite and 1 doc-test) and `cargo doc` with `RUSTDOCFLAGS="-D warnings"`.
