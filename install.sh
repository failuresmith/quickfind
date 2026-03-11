#!/usr/bin/env bash
set -euo pipefail

echo "quickfind installer"
echo "=================="

if ! command -v quickfind >/dev/null 2>&1; then
  echo "Error: quickfind is not in PATH."
  echo "Install first with: cargo install quickfind"
  exit 1
fi

echo
echo "Step 1/4: interactive config onboarding"
echo "We will ask which directories you want quickfind to index."
quickfind --init

echo
echo "Step 2/4: initial index build"
quickfind --index

echo
read -r -p "Step 3/4: Enable always-on watcher daemon via systemd user service? [Y/n] " enable_daemon
enable_daemon="${enable_daemon:-Y}"

if [[ "$enable_daemon" =~ ^([Yy]|[Yy][Ee][Ss])$ ]]; then
  systemd_user_dir="${HOME}/.config/systemd/user"
  service_file="${systemd_user_dir}/quickfind-watcher.service"
  quickfind_bin="$(command -v quickfind)"

  mkdir -p "$systemd_user_dir"

  cat >"$service_file" <<EOF_SERVICE
[Unit]
Description=quickfind watcher daemon
After=default.target

[Service]
Type=simple
ExecStart=${quickfind_bin} --watch
Restart=on-failure
RestartSec=2
Nice=19
IOSchedulingClass=idle

[Install]
WantedBy=default.target
EOF_SERVICE

  systemctl --user daemon-reload
  systemctl --user enable --now quickfind-watcher.service

  echo "Daemon enabled: quickfind-watcher.service"
  echo "Logs: journalctl --user -u quickfind-watcher.service -f"
else
  echo "Skipping daemon setup. You can still run: quickfind --watch"
fi

echo
echo "Step 4/4: done"
echo "Try: quickfind <query>"
