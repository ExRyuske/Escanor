#!/usr/bin/env bash
# Собирает пакет для Windows: dist/release/escanor-<версия>-windows-x86_64.zip и .sha256.
#
# Один и тот же путь у `make package` и у CI — иначе сборка «работает у меня»
# и падает на сервере по причине, которой в workflow не видно. На Windows
# собирается напрямую, на macOS/Linux — через cargo-xwin.
#
# В архиве всё содержимое папки программы: exe, DLL веб-камеры, APK для телефона
# и adb. Его же скачивает встроенное обновление, сверяя с файлом .sha256.
# APK берётся из $APK (в CI — артефакт задачи android), иначе собирается Gradle.
set -euo pipefail

cd "$(dirname "$0")/.."

VERSION="$(sed -n '/^\[workspace\.package\]/,/^\[/ s/^version = "\([^"]*\)".*/\1/p' Cargo.toml | head -1)"
test -n "$VERSION"
NAME="escanor-$VERSION-windows-x86_64"

if [ -z "${APK:-}" ]; then
    (cd android && ./gradlew -q assembleRelease)
    APK=android/app/build/outputs/apk/release/app-release.apk
fi
test -f "$APK"

case "$(uname -s)" in
    MINGW* | MSYS* | CYGWIN* | Windows_NT)
        cargo build --release --locked -p escanor -p escanor-vcam
        BIN=target/release
        ;;
    *)
        cargo xwin build --release --locked --target x86_64-pc-windows-msvc -p escanor -p escanor-vcam
        BIN=target/x86_64-pc-windows-msvc/release
        ;;
esac

STAGE="dist/$NAME"
rm -rf "$STAGE"
mkdir -p "$STAGE" dist/release
cp "$BIN/escanor.exe" "$BIN/escanor_vcam.dll" "$STAGE/"
cp "$APK" "$STAGE/escanor.apk"

# adb из официальных platform-tools: рядом с программой его находит Adb::locate.
TOOLS=target/platform-tools-windows.zip
if [ ! -f "$TOOLS" ]; then
    curl -fsSL -o "$TOOLS" https://dl.google.com/android/repository/platform-tools-latest-windows.zip
fi
unzip -q -o -j "$TOOLS" 'platform-tools/adb.exe' 'platform-tools/AdbWinApi.dll' 'platform-tools/AdbWinUsbApi.dll' -d "$STAGE"

ZIP="$PWD/dist/release/$NAME.zip"
rm -f "$ZIP"
if command -v zip > /dev/null; then
    (cd "$STAGE" && zip -q -9 -r "$ZIP" .)
else
    (cd "$STAGE" && 7z a -tzip -mx=9 "$ZIP" . > /dev/null)
fi
(cd dist/release && sha256sum "$NAME.zip" > "$NAME.zip.sha256" 2> /dev/null || shasum -a 256 "$NAME.zip" > "$NAME.zip.sha256")

echo "готово: dist/release/$NAME.zip"
ls -la dist/release
