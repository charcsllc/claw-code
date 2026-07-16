# Coverage ratchet

[`coverage-floor.txt`](coverage-floor.txt) holds a single integer: the minimum
percentage of **line coverage** the Rust workspace must keep. The `coverage`
job in [`workflows/rust-ci.yml`](workflows/rust-ci.yml) enforces it with:

```
cargo llvm-cov --workspace --summary-only --fail-under-lines <floor>
```

run from `rust/`, where `<floor>` is the contents of
`.github/coverage-floor.txt`.

## The rule: the floor only goes UP

- Never lower the number to turn a red build green. If coverage dropped, add
  tests (or remove dead code) instead.
- After a change that meaningfully improves coverage, raise the floor to the
  current workspace line-coverage percentage **rounded down** to an integer.
  Check the current value locally from `rust/` with
  `cargo llvm-cov --workspace --summary-only`.
- Small floor bumps often are the point: each merge locks in the progress so
  coverage can never silently regress below the best level already reached.

## Current status

The initial floor is a conservative `50`, and the CI job is
`continue-on-error: true` because the real workspace percentage has not been
measured in CI yet. Once the job passes in green: delete the
`continue-on-error` line to make it blocking, and set the floor to the
measured percentage rounded down.
