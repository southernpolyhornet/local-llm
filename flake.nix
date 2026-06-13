{
  description = "local-llm: NixOS-geared local LLM management (llama-swap + llama.cpp)";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, flake-utils }:
    let
      # Overlay exposing `local-llm` so the NixOS module can pick it up.
      overlay = final: _prev: {
        local-llm = final.callPackage ./nix/package.nix { src = self; };
      };
    in
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [ overlay ];
        };
      in
      {
        packages = {
          default = pkgs.local-llm;
          local-llm = pkgs.local-llm;
        };

        apps.default = {
          type = "app";
          program = "${pkgs.local-llm}/bin/local-llm";
        };

        devShells.default = pkgs.mkShell {
          name = "local-llm-dev";
          packages = with pkgs; [
            cargo
            rustc
            clippy
            rustfmt
            rust-analyzer
            gcc
            llama-swap
            jq
            nixpkgs-fmt
          ];
          RUST_SRC_PATH = "${pkgs.rustPlatform.rustLibSrc}";
        };

        checks = {
          inherit (self.packages.${system}) local-llm;
        };

        formatter = pkgs.nixpkgs-fmt;
      })
    // {
      overlays.default = overlay;

      nixosModules.local-llm = {
        imports = [ ./nix/module.nix ];
        nixpkgs.overlays = [ overlay ];
      };
      nixosModules.default = self.nixosModules.local-llm;
    };
}
