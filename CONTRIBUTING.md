# Contributing to suzume-sql

Thanks for your interest in contributing! suzume-sql is a small project and contributions of any size - bug reports, fixes, features, docs or even comments about the application - are welcome.

## Before you contribute

suzume-sql is **open source**, licensed under the [GNU General Public License v3.0 or later](LICENSE). By submitting a contribution you agree that it is licensed under the same terms (inbound = outbound); you retain the copyright to your work.

## Sign your work (DCO)

We use the [Developer Certificate of Origin](https://developercertificate.org/): a one-line certification that you wrote the contribution, or otherwise have the right to submit it under the project's license. You don't sign any separate document — you just add a `Signed-off-by` line to each commit, which git does for you with the `-s` flag:

```sh
git commit -s -m "fix: handle empty result sets"
```

This adds a trailer using your git `user.name` and `user.email`:

```
Signed-off-by: Your Name <you@example.com>
```

Forgot to sign off? You can sign off the whole branch at once and update your PR:

```sh
git rebase --signoff main
git push --force-with-lease
```

(For a single commit, `git commit --amend -s` is enough.)

## Development setup

You'll need a recent stable Rust toolchain ([rustup](https://rustup.rs/) is the easiest way to get one).

```sh
git clone https://github.com/Alexrp02/suzume-sql.git
cd suzume-sql
cargo run                         # run the app (uses the OS config dir picker)
cargo run -- --config example-config.toml   # run with the sample config
```

## Code style

- Format: `cargo fmt --all`
- Lint: `cargo clippy --all-targets -- -D warnings`
- Keep commits focused; follow the existing commit message style (e.g. `feat:`, `fix:`, `chore:`).

CI runs `fmt --check`, `clippy`, and `test` on every pull request, so please run them locally before pushing (PR's have to pass all of them to be merged).

## Submitting changes

1. Open an issue for non-trivial changes so we can discuss the approach first.
2. Fork the repo and create a branch off `main`.
3. Make your change with valuable tests (we don't want 1+1 unit tests)
4. Ensure `cargo fmt`, `cargo clippy`, and `cargo test` pass.
5. Sign off your commits with `-s` (see [Sign your work](#sign-your-work-dco)).
6. Open a pull request describing **what** changed and **why**.

## Reporting bugs

Open a GitHub issue with the suzume-sql version (`suzume --version` if available, or the commit), your OS, the database engine, and steps to reproduce. Please do not include real credentials in bug reports.
