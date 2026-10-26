# b33p's tasks; `just` lists them. `just check` runs the commands CI runs
# (.github/workflows/ci.yml, kept in step by hand), and the test box's own workspaces' too,
# which CI doesn't.

set shell := ["bash", "-euo", "pipefail", "-c"]

[private]
default:
    @just --list

# The client, against Signal.
[group('run')]
run:
    cargo run --release

# The offline fake: nothing is sent.
[group('run')]
mock:
    B33P_MOCK=1 cargo run

[group('check')]
fmt:
    cargo fmt --all
    just each fmt --all

# Formatting and clippy, as CI checks them (b33p-core also without its real backend, as the
# mock-only UI and xtask build it), and the test box's workspaces.
[group('check')]
lint:
    cargo fmt --all -- --check
    cargo clippy --workspace --all-targets --locked -- -D warnings
    cargo clippy -p b33p-core --all-targets --locked --features test-servers -- -D warnings
    cargo clippy -p b33p-core --all-targets --locked --no-default-features -- -D warnings
    just each fmt --all -- --check
    just each clippy --all-targets --all-features --locked -- -D warnings

# The unit tests and the window's own behaviour; the test box's workspaces' tests.
[group('check')]
test:
    cargo test --workspace --locked
    just each test --all-features --locked

# SQLCipher's crypto in pure Rust: BoringSSL can't be linked next to OpenSSL's libcrypto (ci.yml).
[group('check')]
sqlcipher:
    cargo tree --locked -e features -i libsqlite3-sys | grep bundled-sqlcipher-custom-crypto >/dev/null

# Licenses, advisories and sources (deny.toml).
[group('check')]
deny:
    cargo deny --locked check

# The workflows' security (zizmor, as CI runs it).
[group('check')]
zizmor:
    zizmor .github/workflows

# What CI checks: first what builds nothing, so that its failures show in seconds.
[group('check')]
check: sqlcipher deny zizmor lint test

# `cargo <command> [arguments]` in each of the test box's workspaces of their own
# (testbox/*/Cargo.toml), into one target directory: what they share builds once.
[private]
each command *arguments:
    for manifest in testbox/*/Cargo.toml; do \
        CARGO_TARGET_DIR=target/testbox-workspaces \
            cargo {{ command }} --manifest-path "$manifest" {{ arguments }}; \
    done

# The docs, warnings as errors.
[group('check')]
doc:
    RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --document-private-items --locked

# End to end over the agent harness, against the mock.
[group('end to end')]
harness:
    cargo test -p b33p-slint --features harness --locked

# The test box's phases on fresh boxes, every one if none is named (testbox/README.md).
[group('end to end')]
testbox *phases:
    cargo testbox run {{ phases }}
