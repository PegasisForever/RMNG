#!/usr/bin/env bash
set -euo pipefail
PLUGIN_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
RUNTIME_ROOT="$PLUGIN_ROOT/.rmng-runtime"
mkdir -p "$RUNTIME_ROOT/bin"
cp "$(command -v node)" "$RUNTIME_ROOT/bin/node"
cat > "$RUNTIME_ROOT/package.json" <<'JSON'
{
  "name": "rmng-pi-runtime",
  "private": true,
  "type": "module",
  "dependencies": {
    "@earendil-works/pi-coding-agent": "0.85.0"
  }
}
JSON
npm ci --ignore-scripts --prefix "$PLUGIN_ROOT"
npm install --ignore-scripts --prefix "$RUNTIME_ROOT"
mkdir -p "$HOME/.local/bin"
cat > "$HOME/.local/bin/pi" <<SCRIPT
#!/usr/bin/env bash
export PATH="$RUNTIME_ROOT/bin:\$PATH"
exec "$RUNTIME_ROOT/bin/node" "$RUNTIME_ROOT/node_modules/@earendil-works/pi-coding-agent/dist/bundle/cli.js" "\$@"
SCRIPT
chmod +x "$HOME/.local/bin/pi"
"$HOME/.local/bin/pi" install "$PLUGIN_ROOT"
