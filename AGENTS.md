# Contributor rules

These apply to people and AI agents. Read the [plan](docs/PLAN.md) section relevant to your change.

- **Rust for application logic.** HTML, CSS, SQL, configuration and minimal build or platform glue
  keep their own formats.
- **Check every change.** Run `./tools/check.sh`: formatting, clippy with warnings as errors, tests
  and docs. Report failures honestly; a change is done when its checks pass.
- **No guessing.** Do not claim a cause, fix or measurement you have not shown with a test or a run.
  Label untested ideas as untested.
- **Fictional data only.** Never commit real member data, credentials, certificates or keys.
- **Wire changes are versioned.** Bump the protocol version in `crates/itson-protocol/src/version.rs`;
  do not edit an exact-JSON test just to make it pass.
- **Dependencies are deliberate.** Propose a new crate in the pull request with its exact version and
  why it is needed. Record it in `DEPENDENCIES.md` and commit `Cargo.lock`.
- **No `unsafe`, `unwrap` or `expect`** outside tests. Keep GUI code thin; state and logic belong in
  toolkit-independent crates.
- **People first.** Keyboard and screen-reader access, readable text, reduced motion and text
  alternatives to voice are requirements. Interface text is plain and literal where it matters:
  buttons, errors, permissions, privacy and safety.
- **Privacy by default.** Hosts choose where data lives. No telemetry, no third-party calls, and no
  feature that needs a member's location, contacts or real identity.
