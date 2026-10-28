#!/usr/bin/env bash
set -eo pipefail

# FFF MCP Server installer
# Usage: curl -fsSL https://raw.githubusercontent.com/dmtrKovalenko/fff/main/install-mcp.sh | bash

REPO="dmtrKovalenko/fff"
BINARY_NAME="fff-mcp"
INSTALL_DIR="${FFF_MCP_INSTALL_DIR:-$HOME/.local/bin}"

PINNED_RELEASE_TAG="v0.11.0"

SHA256_X86_64_UNKNOWN_LINUX_MUSL="d1e8671f40c89c445aaa1728a21ec04b74be2e5f99091485caeabcc97f99aceb"
SHA256_AARCH64_UNKNOWN_LINUX_MUSL="35f8e05d6c865266bf451d13bfa5e1b0d98438fda7dc1c9bdbc1c6a83484577d"
SHA256_X86_64_APPLE_DARWIN="33377a8f96442ebc8b06d3bd2863cf66a520f97a33700e0fb85a09dd7988e862"
SHA256_AARCH64_APPLE_DARWIN="cc606d9418cfa7764376dcd7b32a33e6459a6cb9a4718be366b024dca6e9fead"
SHA256_X86_64_PC_WINDOWS_MSVC="7a40439b06a33880487661b519f116a6a6d48b1c684bce79adfd5666875f23fd"
SHA256_AARCH64_PC_WINDOWS_MSVC="6525ac801df272906f372502707f4a9812c440ace5549ddc7810570d4ca5aced"

expected_sha_for() {
    case "$1" in
        x86_64-unknown-linux-musl)  echo "$SHA256_X86_64_UNKNOWN_LINUX_MUSL" ;;
        aarch64-unknown-linux-musl) echo "$SHA256_AARCH64_UNKNOWN_LINUX_MUSL" ;;
        x86_64-apple-darwin)        echo "$SHA256_X86_64_APPLE_DARWIN" ;;
        aarch64-apple-darwin)       echo "$SHA256_AARCH64_APPLE_DARWIN" ;;
        x86_64-pc-windows-msvc)     echo "$SHA256_X86_64_PC_WINDOWS_MSVC" ;;
        aarch64-pc-windows-msvc)    echo "$SHA256_AARCH64_PC_WINDOWS_MSVC" ;;
    esac
}

info() { printf '\033[1;34m%s\033[0m\n' "$*"; }
success() { printf '\033[1;38;5;208m%s\033[0m\n' "$*"; }
warn() { printf '\033[1;33m%s\033[0m\n' "$*"; }
error() { printf '\033[1;31mError: %s\033[0m\n' "$*" >&2; exit 1; }

# Print JSON with syntax highlighting via jq if available, plain otherwise
print_json() {
    if command -v jq &>/dev/null; then
        echo "$1" | jq .
    else
        echo "$1"
    fi
}

detect_platform() {
    local os arch target

    os="$(uname -s)"
    arch="$(uname -m)"

    case "$os" in
        Linux)
            # Prefer musl (static) for maximum compatibility
            case "$arch" in
                x86_64)  target="x86_64-unknown-linux-musl" ;;
                aarch64|arm64) target="aarch64-unknown-linux-musl" ;;
                *) error "Unsupported architecture: $arch" ;;
            esac
            ;;
        Darwin)
            case "$arch" in
                x86_64)  target="x86_64-apple-darwin" ;;
                aarch64|arm64) target="aarch64-apple-darwin" ;;
                *) error "Unsupported architecture: $arch" ;;
            esac
            ;;
        MINGW*|MSYS*|CYGWIN*)
            case "$arch" in
                x86_64)  target="x86_64-pc-windows-msvc" ;;
                aarch64|arm64) target="aarch64-pc-windows-msvc" ;;
                *) error "Unsupported architecture: $arch" ;;
            esac
            ;;
        *) error "Unsupported OS: $os" ;;
    esac

    echo "$target"
}

get_latest_release_tag() {
    local target="$1"

    # Honor the pin baked in by `make bump-install-mcp-sh`. Required when SHAs
    # are pinned, since fetching /releases would race against newer releases.
    if [ -n "$PINNED_RELEASE_TAG" ]; then
        echo "$PINNED_RELEASE_TAG"
        return
    fi

    local releases_json
    local curl_args=(-fsSL)

    # Use gh CLI token if available to avoid rate limiting
    if command -v gh &>/dev/null; then
        local gh_token
        gh_token="$(gh auth token 2>/dev/null || true)"
        if [ -n "$gh_token" ]; then
            curl_args+=(-H "Authorization: token $gh_token")
        fi
    fi

    releases_json=$(curl "${curl_args[@]}" "https://api.github.com/repos/${REPO}/releases") \
        || error "Failed to fetch releases from https://github.com/${REPO}/releases"

    # Find the first release that contains an fff-mcp binary for our platform
    local tag
    tag=$(echo "$releases_json" \
        | grep -oE '"(tag_name|name)": *"[^"]*"' \
        | awk -v target="fff-mcp-${target}" '
            /"tag_name":/ { gsub(/.*": *"|"/, ""); current_tag = $0; next }
            /"name":/ && index($0, target) { print current_tag; exit }
        ')

    if [ -z "$tag" ]; then
        error "No release found containing fff-mcp binaries for ${target}. The MCP build may not have been released yet."
    fi
    echo "$tag"
}

download_binary() {
    local target="$1"
    local tag="$2"
    local ext=""

    case "$target" in
        *windows*) ext=".exe" ;;
    esac

    local filename="${BINARY_NAME}-${target}${ext}"
    local url="https://github.com/${REPO}/releases/download/${tag}/${filename}"
    local checksum_url="${url}.sha256"

    info "Downloading ${filename} from release ${tag}..."

    local tmp_dir
    tmp_dir="$(mktemp -d)"
    trap 'rm -rf "$tmp_dir"' EXIT

    if ! curl -fsSL -o "${tmp_dir}/${filename}" "$url" 2>/dev/null; then
        echo "" >&2
        printf '\033[1;31mError: Failed to download binary for your platform.\033[0m\n' >&2
        echo "" >&2
        echo "  URL: ${url}" >&2
        echo "  Release: ${tag}" >&2
        echo "  Platform: ${target}" >&2
        echo "" >&2
        echo "This likely means the MCP binary hasn't been built for this release yet." >&2
        echo "Check available releases at: https://github.com/${REPO}/releases" >&2
        exit 1
    fi

    # Verify checksum: prefer the SHA pinned in this script (offline, tamper-evident).
    # Fall back to the .sha256 file on the release for targets/releases without a pin.
    if command -v sha256sum &>/dev/null; then
        local pinned_sha
        pinned_sha="$(expected_sha_for "$target")"
        if [ -n "$pinned_sha" ]; then
            info "Verifying checksum against pinned value..."
            echo "${pinned_sha}  ${filename}" > "${tmp_dir}/${filename}.sha256"
            (cd "$tmp_dir" && sha256sum -c "${filename}.sha256") \
                || error "Checksum verification failed!"
        elif curl -fsSL -o "${tmp_dir}/${filename}.sha256" "$checksum_url" 2>/dev/null; then
            info "Verifying checksum..."
            (cd "$tmp_dir" && sha256sum -c "${filename}.sha256") \
                || error "Checksum verification failed!"
        else
            warn "Checksum file not available, skipping verification."
        fi
    fi

    # Install
    mkdir -p "$INSTALL_DIR"
    mv "${tmp_dir}/${filename}" "${INSTALL_DIR}/${BINARY_NAME}${ext}"
    chmod +x "${INSTALL_DIR}/${BINARY_NAME}${ext}"

    if [ "$IS_UPDATE" != true ]; then
        success "Installed ${BINARY_NAME} to ${INSTALL_DIR}/${BINARY_NAME}${ext}"
    fi
}

check_path() {
    case ":$PATH:" in
        *":${INSTALL_DIR}:"*) return 0 ;;
    esac

    warn "${INSTALL_DIR} is not in your PATH."
    echo ""
    echo "Add it to your shell profile:"
    echo ""

    local shell_name
    shell_name="$(basename "${SHELL:-bash}")"
    case "$shell_name" in
        zsh)
            echo "  echo 'export PATH=\"${INSTALL_DIR}:\$PATH\"' >> ~/.zshrc"
            echo "  source ~/.zshrc"
            ;;
        fish)
            echo "  fish_add_path ${INSTALL_DIR}"
            ;;
        *)
            echo "  echo 'export PATH=\"${INSTALL_DIR}:\$PATH\"' >> ~/.bashrc"
            echo "  source ~/.bashrc"
            ;;
    esac
    echo ""
}

print_setup_instructions() {
    local binary_path="${INSTALL_DIR}/${BINARY_NAME}"
    local found_any=false

    echo ""
    success "FFF MCP Server installed successfully!"
    echo ""
    info "Setup with your AI coding assistant:"
    echo ""

    # Claude Code
    if command -v claude &>/dev/null; then
        found_any=true
        success "[Claude Code] detected"
        echo ""
        echo "Global (recommended):"
        echo "claude mcp add -s user fff -- ${binary_path}"
        echo ""
        echo "Or project-level .mcp.json (uses PATH):"
        echo ""
        print_json '{
  "mcpServers": {
    "fff": {
      "type": "stdio",
      "command": "fff-mcp",
      "args": []
    }
  }
}'
        echo ""
    fi

    # OpenCode
    if command -v opencode &>/dev/null; then
        found_any=true
        success "[OpenCode] detected"
        echo ""
        echo "Add to ~/.config/opencode/opencode.json:"
        echo ""
        print_json '{
  "mcp": {
    "fff": {
      "type": "local",
      "command": ["fff-mcp"],
      "enabled": true
    }
  }
}'
        echo ""
    fi

    # Codex
    if command -v codex &>/dev/null; then
        found_any=true
        success "[Codex] detected"
        echo ""
        echo "codex mcp add fff -- \"${binary_path}\""
        echo ""
    fi

    if [ "$found_any" = false ]; then
        echo "No AI coding assistants detected."
        echo ""
        echo "Binary path: ${binary_path}"
        echo ""
    fi

    echo "Binary: ${binary_path}"
    echo "Docs:   https://github.com/${REPO}"
    echo ""
    info "Tip: Add this to your CLAUDE.md or AGENTS.md to make AI use fff for all searches:"
    echo "\""
    echo "Use the fff MCP tools for all file search operations instead of default tools."
    echo "\""


}

main() {
    local target
    target="$(detect_platform)"

    local existing_binary="${INSTALL_DIR}/${BINARY_NAME}"
    IS_UPDATE=false

    if [ -x "$existing_binary" ]; then
        IS_UPDATE=true
        info "Updating FFF MCP Server..."
    else
        info "Installing FFF MCP Server..."
    fi
    echo ""

    info "Detected platform: ${target}"

    local tag
    tag="$(get_latest_release_tag "$target")"

    download_binary "$target" "$tag"

    if [ "$IS_UPDATE" = true ]; then
        echo ""
        success "FFF MCP Server updated to ${tag}!"
        echo ""
    else
        check_path
        print_setup_instructions
    fi
}

main
