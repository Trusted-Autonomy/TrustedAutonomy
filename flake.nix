{
  description = "Trusted Autonomy — local-first agent substrate";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, rust-overlay, flake-utils, ... }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs {
          inherit system overlays;
        };

        # Read toolchain from rust-toolchain.toml so Nix and rustup stay in sync
        rustToolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;

        # Native build inputs needed for compilation (C linker, pkg-config)
        nativeBuildInputs = with pkgs; [
          pkg-config
        ];

        # Libraries needed at build and runtime
        buildInputs = with pkgs; [
          openssl
        ] ++ pkgs.lib.optionals pkgs.stdenv.isDarwin [
          pkgs.libiconv
        ];

      in
      let
        # Build the ta-cli package once
        ta-cli-package = pkgs.rustPlatform.buildRustPackage {
          pname = "ta-cli";
          version = "0.1.0-alpha";

          src = ./.;

          cargoLock = {
            lockFile = ./Cargo.lock;
          };

          inherit nativeBuildInputs buildInputs;

          # Build only ta-cli, not all workspace members
          cargoBuildFlags = [ "-p" "ta-cli" ];

          meta = with pkgs.lib; {
            description = "Trusted Autonomy — local-first agent substrate";
            homepage = "https://github.com/trustedautonomy/ta";
            license = licenses.asl20;
            maintainers = [ ];
            mainProgram = "ta";
          };
        };
      in {
        # Packages
        packages = {
          default = ta-cli-package;
          ta-cli = ta-cli-package;
        };

        # Apps for 'nix run github:trustedautonomy/ta'
        apps = {
          default = {
            type = "app";
            program = "${pkgs.lib.getExe ta-cli-package}";
          };
          ta = {
            type = "app";
            program = "${pkgs.lib.getExe ta-cli-package}";
          };
        };

        # Development shell
        devShells.default = pkgs.mkShell {
          inherit nativeBuildInputs buildInputs;

          packages = with pkgs; [
            # Rust toolchain (compiler, cargo, rustfmt, clippy, rust-analyzer)
            rustToolchain

            # Version control
            git

            # Dev tools
            cargo-nextest  # faster test runner with better output
            just           # command runner (like make, but simpler)
            imagemagick    # icon generation (convert PNG → ICO, resize)
          ];

          shellHook = ''
            echo "Trusted Autonomy dev environment loaded"
            echo "  Rust: $(rustc --version)"
            echo "  Cargo: $(cargo --version)"
            echo ""
            echo "Quick start:"
            echo "  just build   — build all crates"
            echo "  just test    — run all tests"
            echo "  just check   — lint + format check"
          '';
        };
      }
    );
}
