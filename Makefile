# CleanSweep 发版与构建入口
#
# 用法：
#   make tag v0.0.3    # 改版本号 → 提交 → 推送分支 → 打 tag → 推送 tag（触发 GitHub Actions）
#   make build         # 本地构建前端 + Tauri 安装包（NSIS / MSI）
#   make check         # 前端构建 + Rust 单元测试
#
# 需要 GNU Make（Windows 可用 Git Bash 自带的 make，或 `choco install make`）。

VERSION ?= $(word 2,$(MAKECMDGOALS))
.DEFAULT_GOAL := help

.PHONY: help tag build check

help:
	@echo CleanSweep Makefile
	@echo   make tag v0.0.3   更新版本号、提交、打 tag 并推送，触发 GitHub Actions 构建
	@echo   make build        本地构建前端 + Tauri 安装包（NSIS / MSI）
	@echo   make check        前端构建 + Rust 单元测试
	@echo   要求：Git、Node/pnpm、Rust；Windows 上建议使用 Git Bash，或安装 GNU Make。

tag:
	powershell -NoProfile -ExecutionPolicy Bypass -File tools/tag-release.ps1 -Version "$(VERSION)" $(if $(DRY_RUN),-DryRun)

build:
	pnpm tauri build

check:
	pnpm build
	cd src-tauri && cargo test --lib

# 允许 “make tag v0.0.3” 把版本号当作额外目标而不报错
%:
	@:
