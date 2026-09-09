#!/usr/bin/env python3
"""Generate ruzu's context-free GTK translation tables from Qt TS catalogs."""

from __future__ import annotations

import argparse
import json
import xml.etree.ElementTree as ET
from collections import OrderedDict
from pathlib import Path


FRENCH_PREFERRED_TRANSLATIONS = {
    "Audio": "Audio",
    "General": "Général",
    "Graphics": "Graphismes",
    "Advanced": "Avancé",
    "Game List": "Liste des jeux",
    "None": "Aucun",
}

# ruzu uses a shorter GTK button label than upstream's
# "Add New Game Directory" so it remains compact at narrow window sizes.
SHORT_GAME_DIRECTORY_TRANSLATIONS = {
    "ar": "إضافة مجلد ألعاب",
    "ca": "Afegir directori de jocs",
    "cs": "Přidat složku s hrami",
    "da": "Tilføj spilmappe",
    "de": "Spieleverzeichnis hinzufügen",
    "el": "Προσθήκη τοποθεσίας παιχνιδιών",
    "es": "Añadir directorio de juegos",
    "fi": "Lisää pelikansio",
    "fr": "Ajouter un répertoire de jeux",
    "hu": "Játékkönyvtár hozzáadása",
    "id": "Tambahkan direktori permainan",
    "it": "Aggiungi cartella dei giochi",
    "ja_JP": "ゲームディレクトリを追加",
    "ko_KR": "게임 디렉터리 추가",
    "nb": "Legg til spillmappe",
    "nl": "Spelmap toevoegen",
    "pl": "Dodaj katalog gier",
    "pt_BR": "Adicionar pasta de jogos",
    "pt_PT": "Adicionar diretório de jogos",
    "ru_RU": "Добавить папку с играми",
    "sv": "Lägg till spelkatalog",
    "tr_TR": "Oyun konumu ekle",
    "uk": "Додати папку з іграми",
    "vi": "Thêm thư mục game",
    "vi_VN": "Thêm thư mục game",
    "zh_CN": "添加游戏目录",
    "zh_TW": "加入遊戲資料夾",
}

UPSTREAM_MISSING_KEYS_DETAIL = "Encryption keys are missing. <br>Please follow <a href='https://yuzu-emu.org/help/quickstart/'>the yuzu quickstart guide</a> to get all your keys, firmware and games."
RUZU_MISSING_KEYS_DETAIL = "Encryption keys are missing. <br>Please follow <a href='https://yuzu-mirror.github.io/help/quickstart/'>the ruzu quickstart guide</a> to install your keys and firmware, then add your games."


def read_catalog(path: Path, rust_text: str, context: str | None = None) -> OrderedDict[str, str]:
    messages: OrderedDict[str, str] = OrderedDict()
    root = ET.parse(path).getroot()
    contexts = [item for item in root.findall("context")
                if context is None or item.findtext("name") == context]
    for message in (message for item in contexts for message in item.findall("message")):
        source = message.findtext("source")
        translation = message.find("translation")
        if not source or translation is None:
            continue
        if translation.get("type") in {"unfinished", "vanished", "obsolete"}:
            continue
        translated = "".join(translation.itertext())
        if "Eden" in source and source.replace("Eden", "Ruzu") in rust_text:
            source = source.replace("Eden", "Ruzu")
            translated = translated.replace("Eden", "Ruzu")
        if source == UPSTREAM_MISSING_KEYS_DETAIL and RUZU_MISSING_KEYS_DETAIL in rust_text:
            messages[RUZU_MISSING_KEYS_DETAIL] = (
                translated
                .replace("https://yuzu-emu.org/help/quickstart/", "https://yuzu-mirror.github.io/help/quickstart/")
                .replace("yuzu", "ruzu")
                .replace("Yuzu", "Ruzu")
            )
        candidates = {
            source,
            source.replace("&", "_"),
            source.replace("yuzu", "ruzu").replace("Yuzu", "Ruzu"),
            source.replace("&", "_").replace("yuzu", "ruzu").replace("Yuzu", "Ruzu"),
        }
        if translated and any(candidate in rust_text for candidate in candidates):
            messages.setdefault(source, translated)
    if context is None and path.stem == "fr":
        messages.update(FRENCH_PREFERRED_TRANSLATIONS)
    if context is None and "Add Game Directory" in rust_text:
        short_translation = SHORT_GAME_DIRECTORY_TRANSLATIONS.get(path.stem)
        if short_translation:
            messages["Add Game Directory"] = short_translation
    return messages


def write_tables(path: Path, catalogs: Path, rust_source: Path, context: str | None = None) -> None:
    rust_text = "\n".join(
        source.read_text(encoding="utf-8", errors="ignore")
        for source in rust_source.rglob("*.rs")
        if source.resolve() != path.resolve()
        and not source.name.startswith("i18n_")
    )
    tables = {
        catalog.stem: read_catalog(catalog, rust_text, context)
        for catalog in sorted(catalogs.glob("*.ts"))
    }
    if context is not None:
        # Import only absent entries from this context. Preserve existing
        # context-free translations and all unrelated manually maintained keys.
        existing = json.loads(path.read_text(encoding="utf-8")) if path.exists() else {}
        for locale, messages in tables.items():
            for source, translated in messages.items():
                existing.setdefault(locale, {}).setdefault(source, translated)
        tables = existing
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(
        json.dumps(tables, ensure_ascii=False, separators=(",", ":"), sort_keys=True),
        encoding="utf-8",
    )


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("catalogs", type=Path)
    parser.add_argument("output", type=Path)
    parser.add_argument("--rust-source", type=Path, required=True)
    parser.add_argument("--context", help="Merge missing messages from one Qt context only")
    args = parser.parse_args()
    write_tables(args.output, args.catalogs, args.rust_source, args.context)


if __name__ == "__main__":
    main()
