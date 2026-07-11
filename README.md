# clickhouse-query-ext

[![Rust CI](https://github.com/QueryaHub/clickhouse-query-ext/actions/workflows/rust.yml/badge.svg)](https://github.com/QueryaHub/clickhouse-query-ext/actions/workflows/rust.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)

Расширение базы данных [ClickHouse](https://clickhouse.com/) для [Querya Desktop](https://github.com/QueryaHub). Устанавливается как `.qext`-пакет и добавляет ClickHouse в список подключений: форма настройки, дерево схемы, просмотр таблиц и SQL-редактор.

## Возможности

- Подключение к ClickHouse по HTTP/HTTPS
- Дерево объектов: базы, таблицы, представления, колонки, партиции
- Выполнение SQL-запросов и просмотр результатов
- Аналитический режим (Safe Mode): ограничение опасных операций и лимиты на сессию
- Контекстные действия для таблиц и партиций (статистика, DDL, optimize и др.)

## Требования

- Querya Desktop 2.0+
- Для сборки из исходников: Rust stable (1.85+)

## Сборка

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
cargo build --release
./scripts/package_qext.sh
```

Артефакты:

| Файл | Назначение |
|------|------------|
| `target/release/clickhouse-query-ext` | Бинарник драйвера |
| `dist/clickhouse-query-ext-1.0.1.qext` | Пакет для установки в Querya Desktop |
| `dist/clickhouse-query-ext-1.0.1.qext.sha256` | Контрольная сумма |

Установка: **Querya Desktop → Extensions → Install from file** → выбрать `.qext`.

## Кросс-компиляция

```bash
./scripts/build_cross.sh x86_64-unknown-linux-gnu
./scripts/package_qext.sh --target x86_64-unknown-linux-gnu
```

Поддерживаемые цели: `x86_64-unknown-linux-gnu`, `aarch64-apple-darwin`, `x86_64-pc-windows-msvc`.

## Релиз

Тег `v*` запускает GitHub Actions: сборка для всех платформ и публикация `.qext` в Releases.

```bash
git tag v1.0.1
git push origin v1.0.1
```

## Документация

Подробная спецификация RPC, SDUI и аналитических функций — в каталоге [`docs/`](docs/).

## Лицензия

MIT
