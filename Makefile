# Короткие команды для типовых задач — те же сочетания флагов, что гоняет CI.

.PHONY: help run apk package check fmt clean

help:
	@echo "make run      — собрать и запустить приложение на этом компьютере"
	@echo "make apk      — собрать и поставить приложение на подключённый телефон"
	@echo "make package  — собрать пакет для Windows в dist/release"
	@echo "make check    — форматирование, clippy и тесты ровно как в CI"
	@echo "make clean    — убрать сборку и пакеты"

run:
	cargo run -p escanor

apk:
	cd android && ./gradlew installDebug

package:
	scripts/package.sh

check:
	cargo fmt --check
	cargo clippy --locked --all-targets -- -D warnings
	cargo test --locked

fmt:
	cargo fmt

clean:
	cargo clean
	cd android && ./gradlew clean
	rm -rf dist
