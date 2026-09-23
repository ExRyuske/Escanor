#!/usr/bin/env python3
"""Выпуск версии: поднять номер во всём workspace.

    python3 scripts/release.py bump --part patch       # версия в Cargo.toml

`bump` меняет общую версию `[workspace.package]` в Cargo.toml и записи пакетов
workspace в Cargo.lock — версии зависимостей не трогает, чтобы `cargo --locked`
оставался честной проверкой. Версию Android-приложения Gradle берёт из того же
Cargo.toml, поэтому ПК и телефон всегда называют одну и ту же версию.
"""

from __future__ import annotations

import argparse
import pathlib
import re

ROOT = pathlib.Path(__file__).resolve().parent.parent

MANIFEST = ROOT / 'Cargo.toml'
LOCKFILE = ROOT / 'Cargo.lock'
PACKAGES = ('escanor', 'escanor-core', 'escanor-shm', 'escanor-vcam')

VERSION = re.compile(r'^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$')


# Кодировку и перевод строки задаём явно: иначе на раннере Windows (cp1252)
# русские комментарии ломают чтение, а запись переводит файл в CRLF.
def read(path: pathlib.Path) -> str:
    return path.read_text(encoding='utf-8')


def write(path: pathlib.Path, text: str) -> None:
    with open(path, 'w', encoding='utf-8', newline='\n') as f:
        f.write(text)


def next_version(version: str, bump: str) -> str:
    match = VERSION.fullmatch(version)
    if not match:
        raise ValueError(f'нужна версия SemVer вида X.Y.Z, получено: {version}')
    major, minor, patch = map(int, match.groups())
    if bump == 'major':
        major, minor, patch = major + 1, 0, 0
    elif bump == 'minor':
        minor, patch = minor + 1, 0
    else:
        patch += 1
    return f'{major}.{minor}.{patch}'


WORKSPACE_VERSION = r'(?ms)(^\[workspace\.package\]\nversion = ")([^"]+)(")'


def workspace_version() -> str:
    match = re.search(WORKSPACE_VERSION, read(MANIFEST))
    if not match:
        raise SystemExit('не нашли [workspace.package] version в Cargo.toml')
    return match.group(2)


def cmd_bump(args: argparse.Namespace) -> None:
    old = workspace_version()
    new = next_version(old, args.part)

    if not args.dry_run:
        text, count = re.subn(WORKSPACE_VERSION, rf'\g<1>{new}\g<3>', read(MANIFEST))
        if count != 1:
            raise SystemExit('не смогли однозначно обновить Cargo.toml')
        write(MANIFEST, text)

        lock = read(LOCKFILE)
        for package in PACKAGES:
            lock, count = re.subn(
                rf'(?m)(^name = "{re.escape(package)}"\nversion = "){re.escape(old)}(")',
                rf'\g<1>{new}\g<2>',
                lock,
            )
            if count != 1:
                raise SystemExit(f'не смогли однозначно обновить {package} в Cargo.lock')
        write(LOCKFILE, lock)
    print(f'v{old} -> v{new}')


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest='command', required=True)

    up = sub.add_parser('bump', help='повысить версию workspace')
    up.add_argument('--part', required=True, choices=('patch', 'minor', 'major'))
    up.add_argument('--dry-run', action='store_true')
    up.set_defaults(run=cmd_bump)

    args = parser.parse_args()
    args.run(args)


if __name__ == '__main__':
    main()
