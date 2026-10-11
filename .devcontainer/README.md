# Dev container

Build and test dependencies for Linux; the host needs only podman or docker. Editors with
dev-container support read `devcontainer.json`. By hand, from the repository root:

    podman build -t b33p-dev .devcontainer
    podman run --rm -it \
        -v "$PWD":/workspaces/beep \
        -v ~/.cargo/registry:/usr/local/cargo/registry \
        -v ~/.cargo/git:/usr/local/cargo/git \
        -w /workspaces/beep -e CARGO_TARGET_DIR=target/devcontainer \
        b33p-dev cargo test --workspace --locked

Cargo caches are shared with the host; the build goes to `target/devcontainer`.
