{ lib
, rustPlatform
, src
,
}:
rustPlatform.buildRustPackage {
  pname = "local-llm";
  version = "0.1.0";

  inherit src;

  cargoLock.lockFile = "${src}/Cargo.lock";

  # Ship the example config so the NixOS module can seed /etc/local-llm.
  postInstall = ''
    install -Dm644 config/config.example.toml \
      "$out/share/local-llm/config.example.toml"
  '';

  meta = {
    description = "Local LLM management for NixOS (llama-swap + llama.cpp orchestration)";
    longDescription = ''
      Provides the `local-llm` CLI and the `local-llm-resourced` resource
      manager daemon. Together with the bundled NixOS module they wrap
      llama-swap and llama.cpp behind a single, mutable
      /etc/local-llm/config.toml.
    '';
    license = lib.licenses.mit;
    mainProgram = "local-llm";
    platforms = lib.platforms.linux;
  };
}
