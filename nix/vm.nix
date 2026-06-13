# A throwaway NixOS VM that enables `services.local-llm` exactly like a real
# machine would, so you can test the full install flow without touching your
# host system. Built via `nixos-rebuild build-vm --flake .#vm`.
{ lib, pkgs, modulesPath, ... }:
{
  # Pull in the QEMU VM builder + its `virtualisation.*` options (memorySize,
  # diskSize, forwardPorts, ...). Required for `nixos-rebuild build-vm --flake`.
  imports = [ "${modulesPath}/virtualisation/qemu-vm.nix" ];

  # Open WebUI ships under a non-free license, so allow just that package.
  nixpkgs.config.allowUnfreePredicate = pkg: lib.elem (lib.getName pkg) [ "open-webui" ];

  # Minimal bootable, auto-login VM.
  users.users.tester = {
    isNormalUser = true;
    password = "test";
    extraGroups = [ "wheel" ];
  };
  services.getty.autologinUser = "tester";
  security.sudo.wheelNeedsPassword = false;
  networking.firewall.enable = false;
  system.stateVersion = "26.05";

  virtualisation = {
    memorySize = 8192;
    diskSize = 20480;
    # Headless: serial console on the terminal (no GTK window over SSH).
    graphics = false;
    # 4 vCPUs for faster CPU inference (virtualisation.cores does not exist).
    qemu.options = [ "-smp" "4" ];
    # Reach the services from the host browser/curl.
    forwardPorts = [
      { from = "host"; host.port = 8080; guest.port = 8080; } # arbiter API
      { from = "host"; host.port = 8081; guest.port = 8081; } # chat web UI
    ];
  };

  services.local-llm = {
    enable = true;
    acceleration = "cpu";
    webui.enable = true;
    webui.host = "0.0.0.0";

    # Seed a tiny CPU-friendly model so first chat downloads in seconds.
    initialConfigFile = pkgs.writeText "config.toml" ''
      [storage]
      models_dir = "/var/lib/local-llm/models"
      hf_cache_dir = "/var/lib/local-llm/hf"

      [server]
      listen = "0.0.0.0:8080"

      [defaults]
      ttl = 600
      context_size = 4096
      # CPU VM: do not try to offload to a GPU.
      gpu_layers = 0
      flash_attention = false

      [[models]]
      name = "chat"
      description = "Tiny model for VM smoke testing"
      hf_repo = "Qwen/Qwen2.5-Coder-0.5B-Instruct-GGUF"
      hf_file = "qwen2.5-coder-0.5b-instruct-q8_0.gguf"
      context_size = 4096
      aliases = [ "gpt-4o", "default" ]
    '';
  };
}
