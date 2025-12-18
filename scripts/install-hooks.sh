#!/bin/bash

# Install git hooks for the braintrust-sdk-rust project.
# Copies hooks/pre-commit into the repository's hooks directory and tests it.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"

echo "Installing git hooks..."

if [ -d "$PROJECT_ROOT/.git" ]; then
    HOOKS_DIR="$PROJECT_ROOT/.git/hooks"
elif [ -f "$PROJECT_ROOT/.git" ]; then
    GIT_DIR="$(sed -n 's/^gitdir: //p' "$PROJECT_ROOT/.git")"
    if [ -z "$GIT_DIR" ]; then
        echo "Unable to locate git directory." >&2
        exit 1
    fi
    if [[ "$GIT_DIR" != /* ]]; then
        GIT_DIR="$PROJECT_ROOT/$GIT_DIR"
    fi
    HOOKS_DIR="$GIT_DIR/hooks"
else
    echo "This directory does not appear to be a git repository." >&2
    exit 1
fi

mkdir -p "$HOOKS_DIR"

HOOK_SOURCE="$PROJECT_ROOT/hooks/pre-commit"
HOOK_TARGET="$HOOKS_DIR/pre-commit"

if [ ! -f "$HOOK_SOURCE" ]; then
    echo "Hook source not found at $HOOK_SOURCE." >&2
    exit 1
fi

cp "$HOOK_SOURCE" "$HOOK_TARGET"
chmod +x "$HOOK_TARGET"

echo "Pre-commit hook installed."

if git -C "$PROJECT_ROOT" diff --quiet && git -C "$PROJECT_ROOT" diff --cached --quiet; then
    echo "Running hook to verify installation..."
    if "$HOOK_TARGET"; then
        echo "Hook ran successfully."
    else
        echo "Hook failed during verification. Review the output above." >&2
        exit 1
    fi
else
    echo "Skipped hook verification because uncommitted changes are present."
fi

echo "Hooks installation complete."



