# Install the TA Daemon

The VS Code extension connects to a local TA daemon. Install and start it first.

## Install

```bash
bash <(curl -fsSL https://github.com/trustedautonomy/ta/releases/latest/download/install.sh)
```

Or download the binary directly from [GitHub Releases](https://github.com/trustedautonomy/ta/releases).

## Start

```bash
cd your-project
ta start
```

The daemon runs in the background at `http://127.0.0.1:7700`. The status bar at the bottom of VS Code shows **TA: ready** when connected.

## Verify

The TA Goals panel in the sidebar should show **No active goals** (not an error).
