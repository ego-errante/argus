.PHONY: help core agent build typecheck up down

help:
	@echo "Argus targets:"
	@echo "  make core       run the Rust Core (cargo run)"
	@echo "  make agent      run the TS Agent in watch mode"
	@echo "  make build      build Core + install/typecheck Agent"
	@echo "  make up / down  docker-compose up / down"

core:
	cargo run -p argus-core

agent:
	cd agent && npm run dev

build:
	cargo build
	cd agent && npm install && npm run typecheck

typecheck:
	cd agent && npm run typecheck

up:
	docker compose up --build

down:
	docker compose down
