{ config, lib, pkgs, ... }:

let
  cfg = config.services.local-llm;

  # Select the llama.cpp build matching the requested accelerator. Acceleration
  # is a build-time choice (the compiled binary's GPU support cannot change at
  # runtime); per-model GPU layers live in config.toml.
  llamaCppPkg =
    if cfg.acceleration == "cuda" then
      cfg.llamaCppPackage.override { cudaSupport = true; }
    else if cfg.acceleration == "rocm" then
      cfg.llamaCppPackage.override { rocmSupport = true; }
    else if cfg.acceleration == "vulkan" then
      cfg.llamaCppPackage.override { vulkanSupport = true; }
    else
      cfg.llamaCppPackage;

  seedConfig = cfg.initialConfigFile;

  # Launcher reads the generated env file (LOCAL_LLM_LISTEN etc.) at runtime.
  arbiterScript = pkgs.writeShellScript "local-llm-arbiter" ''
    exec ${lib.getExe' cfg.llamaSwapPackage "llama-swap"} \
      --config /run/local-llm/llama-swap.yaml \
      --listen "''${LOCAL_LLM_LISTEN:-127.0.0.1:8080}"
  '';

  daemonEnv = {
    LOCAL_LLM_CONFIG = cfg.configPath;
    LOCAL_LLM_RUNTIME_DIR = "/run/local-llm";
    LOCAL_LLM_LLAMA_SERVER = lib.getExe' llamaCppPkg "llama-server";
    LOCAL_LLM_ARBITER_UNIT = "local-llm-arbiter.service";
  };

  # GPU device access for the arbiter (which spawns llama-server children).
  gpuServiceConfig =
    if cfg.acceleration == "cuda" then {
      PrivateDevices = false;
      DeviceAllow = [
        "/dev/nvidiactl rw"
        "/dev/nvidia-uvm rw"
        "/dev/nvidia-uvm-tools rw"
        "/dev/nvidia0 rw"
      ];
    }
    else if cfg.acceleration == "rocm" || cfg.acceleration == "vulkan" then {
      PrivateDevices = false;
      DeviceAllow = [
        "/dev/dri rw"
        "/dev/kfd rw"
      ];
      SupplementaryGroups = [ "video" "render" ];
    }
    else {
      PrivateDevices = true;
    };
in
{
  options.services.local-llm = {
    enable = lib.mkEnableOption "the local-llm management stack (llama-swap + llama.cpp)";

    package = lib.mkOption {
      type = lib.types.package;
      default = pkgs.local-llm;
      defaultText = lib.literalExpression "pkgs.local-llm";
      description = "The local-llm package providing the CLI and resourced daemon.";
    };

    llamaCppPackage = lib.mkOption {
      type = lib.types.package;
      default = pkgs.llama-cpp;
      defaultText = lib.literalExpression "pkgs.llama-cpp";
      description = "Base llama.cpp package; overridden according to `acceleration`.";
    };

    llamaSwapPackage = lib.mkOption {
      type = lib.types.package;
      default = pkgs.llama-swap;
      defaultText = lib.literalExpression "pkgs.llama-swap";
      description = "The llama-swap package used as the arbiter/front-door.";
    };

    acceleration = lib.mkOption {
      type = lib.types.enum [ "cpu" "cuda" "rocm" "vulkan" ];
      default = "cpu";
      description = ''
        Hardware acceleration backend for llama.cpp. This is a build-time choice
        and selects the appropriate llama.cpp package variant.
      '';
    };

    configPath = lib.mkOption {
      type = lib.types.str;
      default = "/etc/local-llm/config.toml";
      description = "Path to the mutable runtime configuration file.";
    };

    initialConfigFile = lib.mkOption {
      type = lib.types.path;
      default = "${cfg.package}/share/local-llm/config.example.toml";
      defaultText = lib.literalExpression ''"''${cfg.package}/share/local-llm/config.example.toml"'';
      description = ''
        File used to seed {file}`configPath` on first activation only. The
        on-disk file is never overwritten afterwards, so `local-llm configure`
        edits survive rebuilds.
      '';
    };

    dataDir = lib.mkOption {
      type = lib.types.str;
      default = "/var/lib/local-llm";
      description = ''
        Writable directory for model weights and caches. Must contain the
        `models_dir`/`hf_cache_dir` you set in config.toml; if you point those
        elsewhere, add the location to {option}`extraReadWritePaths`.
      '';
    };

    extraReadWritePaths = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [ ];
      example = [ "/mnt/models" ];
      description = "Additional paths the arbiter may write to (e.g. custom model storage).";
    };

    user = lib.mkOption {
      type = lib.types.str;
      default = "local-llm";
      description = "User the arbiter (llama-swap/llama.cpp) runs as.";
    };

    group = lib.mkOption {
      type = lib.types.str;
      default = "local-llm";
      description = "Group the arbiter runs as.";
    };

    port = lib.mkOption {
      type = lib.types.port;
      default = 8080;
      description = ''
        Port to open when {option}`openFirewall` is set. Keep this in sync with
        the `server.listen` port in config.toml.
      '';
    };

    openFirewall = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = "Open {option}`port` in the firewall for the arbiter endpoint.";
    };
  };

  config = lib.mkIf cfg.enable {
    users.users.${cfg.user} = lib.mkIf (cfg.user == "local-llm") {
      isSystemUser = true;
      group = cfg.group;
      description = "local-llm arbiter service user";
      home = cfg.dataDir;
    };
    users.groups.${cfg.group} = lib.mkIf (cfg.group == "local-llm") { };

    environment.systemPackages = [ cfg.package ];

    # Seed the mutable config once (C copies only if the target is absent) and
    # ensure writable data directories exist.
    systemd.tmpfiles.rules = [
      "d /etc/local-llm 0755 root root - -"
      "C ${cfg.configPath} 0644 root root - ${seedConfig}"
      "d ${cfg.dataDir} 0750 ${cfg.user} ${cfg.group} - -"
    ];

    networking.firewall.allowedTCPPorts = lib.mkIf cfg.openFirewall [ cfg.port ];

    systemd.services.local-llm-resourced = {
      description = "local-llm resource manager (config watcher + monitor)";
      wantedBy = [ "multi-user.target" ];
      # systemctl (restart arbiter) + nvidia-smi/rocm-smi (best-effort
      # monitoring, found in the system profile).
      environment = daemonEnv // {
        PATH = "${pkgs.systemd}/bin:/run/current-system/sw/bin";
      };
      serviceConfig = {
        Type = "notify";
        ExecStart = lib.getExe' cfg.package "local-llm-resourced";
        Restart = "on-failure";
        RestartSec = 2;
        RuntimeDirectory = "local-llm";
        RuntimeDirectoryMode = "0755";
        RuntimeDirectoryPreserve = "yes";
        # Runs as root: it reads /etc, writes /run/local-llm and restarts the
        # arbiter via systemctl. Hardened where possible.
        ProtectSystem = "strict";
        ProtectHome = true;
        ReadWritePaths = [ "/run/local-llm" ];
        NoNewPrivileges = true;
        ProtectKernelTunables = true;
        ProtectControlGroups = true;
        RestrictRealtime = true;
      };
    };

    systemd.services.local-llm-arbiter = {
      description = "local-llm arbiter (llama-swap front door)";
      wantedBy = [ "multi-user.target" ];
      after = [ "local-llm-resourced.service" "network.target" ];
      requires = [ "local-llm-resourced.service" ];
      serviceConfig = {
        User = cfg.user;
        Group = cfg.group;
        # Generated by resourced; contains LOCAL_LLM_LISTEN + storage env vars.
        EnvironmentFile = "/run/local-llm/arbiter.env";
        ExecStart = arbiterScript;
        Restart = "on-failure";
        RestartSec = 3;
        # Sandbox; relax just enough for GPU + model storage.
        ProtectSystem = "strict";
        ProtectHome = true;
        PrivateTmp = true;
        NoNewPrivileges = true;
        ReadWritePaths = [ cfg.dataDir ] ++ cfg.extraReadWritePaths;
      } // gpuServiceConfig;
    };
  };
}
