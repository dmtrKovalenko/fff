{
  description = "fff";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";

    crane.url = "github:ipetkov/crane";

    flake-utils.url = "github:numtide/flake-utils";

    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };

    zig-overlay = {
      url = "github:mitchellh/zig-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      crane,
      flake-utils,
      rust-overlay,
      zig-overlay,
      ...
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [ (import rust-overlay) ];
        };

        # zlob requires Zig >= 0.16, but nixpkgs tops out at 0.15. Pull from
        # mitchellh/zig-overlay which ships every released version.
        zig = zig-overlay.packages.${system}."0.16.0";

        rustToolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;

        craneLib = (crane.mkLib pkgs).overrideToolchain rustToolchain;

        cargoToml = builtins.fromTOML (builtins.readFile ./crates/fff-nvim/Cargo.toml);
        fffMcpCargoToml = builtins.fromTOML (builtins.readFile ./crates/fff-mcp/Cargo.toml);

        # Common arguments can be set here to avoid repeating them later
        # Note: changes here will rebuild all dependency crates
        commonArgs = {
          pname = cargoToml.package.name;
          version = cargoToml.package.version;
          # Cargo sources plus the MCP instructions that fff-mcp embeds with include_str!.
          src = pkgs.lib.cleanSourceWith {
            src = ./.;
            name = "source";
            filter =
              path: type:
              pkgs.lib.hasSuffix "/crates/fff-mcp/src/instructions.md" path
              || craneLib.filterCargoSources path type;
          };
          strictDeps = true;

          nativeBuildInputs = [ pkgs.pkg-config pkgs.perl zig pkgs.llvmPackages.libclang.lib ];
          buildInputs = with pkgs; [
            # Add additional build inputs here
            openssl
          ];
          LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";

          # in zlob we compile by default with -target=native unless CI is detected, because 
          # nix is different we have to make sure that the build target is a baseline cpu
          CI = "1";

          # Zig 0.16 insists on writing to its global cache even when the
          # zlob build.rs passes --global-cache-dir. In the nix sandbox $HOME
          # is /homeless-shelter (unwritable), so redirect to $TMPDIR before
          # the build phase runs.
          preBuild = ''
            export ZIG_GLOBAL_CACHE_DIR="$TMPDIR/zig-global-cache"
            export ZIG_LOCAL_CACHE_DIR="$TMPDIR/zig-local-cache"
            export XDG_CACHE_HOME="$TMPDIR/cache"
            mkdir -p "$ZIG_GLOBAL_CACHE_DIR" "$ZIG_LOCAL_CACHE_DIR" "$XDG_CACHE_HOME"
          '';
        };

        my-crate = craneLib.buildPackage (
          commonArgs
          // {
            cargoArtifacts = craneLib.buildDepsOnly commonArgs;
            doCheck = false;
          }
        );

        fff-mcp-args = commonArgs // {
          pname = fffMcpCargoToml.package.name;
          version = fffMcpCargoToml.package.version;
          cargoExtraArgs = "-p fff-mcp --bin fff-mcp";
        };

        fff-mcp = craneLib.buildPackage (
          fff-mcp-args
          // {
            cargoArtifacts = craneLib.buildDepsOnly fff-mcp-args;
            doCheck = false;
          }
        );

        # Copies the dynamic library into the target/release folder
        copy-dynamic-library = /* bash */ ''
          set -eo pipefail
          mkdir -p target/release
          if [ "$(uname)" = "Darwin" ]; then
            cp -vf ${my-crate}/lib/libfff_nvim.dylib target/release/libfff_nvim.dylib
          else
            cp -vf ${my-crate}/lib/libfff_nvim.so target/release/libfff_nvim.so
          fi
          echo "Library copied to target/release/"
        '';
      in
      {
        checks = {
          inherit my-crate fff-mcp;
        };

        packages = {
          default = my-crate;
          inherit fff-mcp;

          # Neovim plugin
          fff-nvim = pkgs.vimUtils.buildVimPlugin {
            pname = "fff.nvim";
            version = "main";
            src = pkgs.lib.cleanSource ./.;
            postPatch = copy-dynamic-library;
            doCheck = false; # Skip require check since we have a Rust FFI component
          };
        };

        apps.default = flake-utils.lib.mkApp {
          drv = my-crate;
        };

        apps.fff-mcp = flake-utils.lib.mkApp {
          drv = fff-mcp;
        };

        # Add the release command
        apps.release = flake-utils.lib.mkApp {
          drv = pkgs.writeShellScriptBin "release" copy-dynamic-library;
        };

        devShells.default = craneLib.devShell {
          # Inherit inputs from checks.
          checks = self.checks.${system};
          # Extra inputs can be added here; cargo and rustc are provided by default.
          packages = [
            # pkgs.ripgrep
          ];
        };
      }
    );
}
