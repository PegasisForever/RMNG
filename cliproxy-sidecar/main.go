// Command cliproxy-sidecar runs exactly one CLIProxyAPI service instance from a
// config path. RMNG's control-server spawns and supervises one of these per
// account group (see crates/control-server/src/cliproxy.rs). It is a thin shell
// around the upstream Go SDK: the control-server owns the config.yaml + auth-dir
// this process points at, drives OAuth onboarding via the management API, and
// reads usage tokens straight out of the auth-dir. The sidecar itself owns only
// protocol execution, account selection, and token refresh (all inside the SDK).
package main

import (
	"context"
	"errors"
	"flag"
	"log"
	"os/signal"
	"path/filepath"
	"syscall"
	"time"

	"github.com/router-for-me/CLIProxyAPI/v7/sdk/cliproxy"
	"github.com/router-for-me/CLIProxyAPI/v7/sdk/config"
)

func main() {
	configPath := flag.String("config", "config.yaml", "path to the instance config.yaml")
	flag.Parse()

	log.SetFlags(log.LstdFlags | log.Lmsgprefix)
	log.SetPrefix("cliproxy-sidecar: ")

	// Absolute path so the SDK's config/auth-dir reload watcher resolves correctly
	// regardless of the process working directory.
	absConfig, err := filepath.Abs(*configPath)
	if err != nil {
		log.Fatalf("resolve config path %q: %v", *configPath, err)
	}

	// Build() requires the parsed config AND the path (the path is only used for
	// hot-reload watching). sdk/config re-exports the loader for embedders.
	cfg, err := config.LoadConfig(absConfig)
	if err != nil {
		log.Fatalf("load config %s: %v", absConfig, err)
	}

	svc, err := cliproxy.NewBuilder().
		WithConfig(cfg).
		WithConfigPath(absConfig).
		WithHooks(cliproxy.Hooks{
			// Readiness marker the Rust supervisor's log drain watches for.
			OnAfterStart: func(*cliproxy.Service) { log.Printf("ready (config=%s)", absConfig) },
		}).
		Build()
	if err != nil {
		log.Fatalf("build failed: %v", err)
	}

	// SIGTERM/SIGINT cancels the context; Run returns and tears the service down.
	// The supervisor stops us by sending SIGTERM (then SIGKILL after a deadline).
	ctx, stop := signal.NotifyContext(context.Background(), syscall.SIGTERM, syscall.SIGINT)
	defer stop()

	if err := svc.Run(ctx); err != nil && !errors.Is(err, context.Canceled) {
		log.Fatalf("run failed: %v", err)
	}

	// Belt-and-suspenders graceful shutdown (idempotent if Run already tore down).
	shutdownCtx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	_ = svc.Shutdown(shutdownCtx)
}
