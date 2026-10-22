setup:
    ln -sf ../../scripts/pre-commit.sh .git/hooks/pre-commit
    @echo "pre-commit hook installed"

fmt:
    cargo fmt --all

fmt-check:
    cargo fmt --all --check

build:
    cargo build

test:
    cargo test
