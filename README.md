# 🚀 clickhouse-query-ext (Querya ClickHouse Database Extension)

[![Rust CI](https://github.com/QueryaHub/clickhouse-query-ext/actions/workflows/rust.yml/badge.svg)](https://github.com/QueryaHub/clickhouse-query-ext/actions/workflows/rust.yml)
[![Rust Edition](https://img.shields.io/badge/Edition-2024-brightgreen.svg)](https://doc.rust-lang.org/edition-guide/rust-2024/)
[![Protocol](https://img.shields.io/badge/Protocol-JSON--RPC%202.0%20over%20NDJSON%20%2F%20stdio-blue.svg)](#architecture)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)

**`clickhouse-query-ext`** — это высокопроизводительный, отказоустойчивый асинхронный драйвер и расширение СУБД [ClickHouse](https://clickhouse.com/) для платформы **Querya Desktop (Analyst Edition)**.  
Драйвер построен на **Rust (Edition 2024)** и работает как изолированный подпроцесс (Zero-Trust Sandbox), взаимодействуя с хостом Querya через протокол **JSON-RPC 2.0 (NDJSON over `stdin` / `stdout`)**.

---

## 📐 Архитектурная схема расширения

```mermaid
graph LR
    subgraph Querya [Querya Desktop Host]
        UI[💻 Generative SDUI]
        Bridge[🔌 Bridge Process Manager]
    end

    subgraph RustSandbox [clickhouse-query-ext (Rust Subprocess)]
        Reader[📥 LineStream stdin] --> Router[🔄 JSON-RPC 2.0 Router]
        Router --> Sys[⚙️ system.* Handshake / Ping / Secrets]
        Router --> Conn[🔌 db.connect / disconnect]
        Router --> Query[📊 db.query / execute / cancel]
        Router --> Sdui[🎨 SDUI Tree & Form Schemas]
        
        Sys --> Pool[🔒 ConnectionSecretsPool zeroize]
        Query --> Safe[🛡️ Safe Mode AST Filter & Limits]
        Sdui --> Parser[🌳 SYSTEM.* Introspection]
        
        Router --> Writer[📤 NDJSON stdout Mutex]
        Writer --> Bridge
    end

    Query -->|HTTP/HTTPS ClickHouse Client| CH[(🌐 ClickHouse Server)]
```

---

## ✨ Ключевые возможности и этапы реализации

Все 6 этапов технического задания полностью реализованы, проверены и покрыты автоматическими тестами:

### 1️⃣ Каркас, асинхронный I/O и NDJSON-транспорт ([Stage 1/6])
- Чтение потока `stdin` через асинхронный `tokio_util::codec::LinesCodec` без блокировки главного потока.
- Запись ответов в `stdout` через потокобезопасный `Mutex<Stdout>` с автоматической очисткой символов перевода строки (`\n`, `\r\n`), гарантирующая 100% валидный **NDJSON (Newline Delimited JSON)**.
- Разделение быстрых методов (`system.*` — задержка `< 5ms`) и тяжёлых SQL-выборок (выполняются в пуле задач `tokio::spawn`).

### 2️⃣ Жизненный цикл, управление секретами и логирование ([Stage 2/6])
- **`system.handshake`**: обмен версиями и регистрация возможностей драйвера (`db.connect`, `db.query`, `db.getSchemaTree`, `sdui.contextActions` и др.).
- **`system.ping`**: Watchdog-таймер мгновенного ответа для предотвращения зависаний (`result: "pong"`).
- **`system.injectCredentials`**: передача паролей и JWT в изолированный `ConnectionSecretsPool`.
- **Защита памяти (`zeroize` & `secrecy`)**: пароли и токены хранятся в защищённой памяти и зануляются при удалении соединения или аварийном завершении (`clear_all`).
- **Санитазированный логгер ([src/utils/logger.rs](src/utils/logger.rs))**: все логи направляются исключительно в `stderr` с автоматическим маскированием паролей и HTTP-заголовков авторизации.

### 3️⃣ Исполнение SQL, конвертация типов и Safe Mode ([Stage 3/6])
- **`db.query` / `db.execute` / `db.cancelQuery`**: выполнение SQL-запросов через HTTP API ClickHouse с поддержкой стримингового парсинга формата `FORMAT JSONCompactEachRowWithNamesAndTypes`.
- **Конвертер типов ([src/mapper/types.rs](src/mapper/types.rs))**: полная поддержка `Int64/UInt64/Int128/UInt256`, `Decimal(P, S)`, `DateTime64`, `Array(T)`, `Tuple(...)`, `Map(K, V)`, `Nullable(T)` и `LowCardinality(T)`. Большие числа автоматически сериализуются в строки (`"18446744073709551615"`), предотвращая потерю точности в JS.
- **🛡️ Аналитический Safe Mode (Read-Only)**:
  - Пре-фильтрация AST на стороне Rust (мгновенная блокировка `DROP DATABASE`, `TRUNCATE TABLE`, `ALTER ... DROP COLUMN` до отправки на сервер).
  - Установка сессионных квот на сервере ClickHouse (`readonly=1`, `max_execution_time=300`, `max_memory_usage=10000000000`).

### 4️⃣ Интроспекция схемы и ленивое дерево ([Stage 4/6])
- **`db.getSchemaTree` & `db.expandTreeNode`**: иерархическая навигация по объектам СУБД с поддержкой ленивой дозагрузки.
- Отображение баз данных (`SYSTEM.databases`), таблиц и представлений (`SYSTEM.tables`), словарей (`SYSTEM.dictionaries` с метриками `HitRate`), колонок (`SYSTEM.columns`) и партиций (`SYSTEM.parts` с расчётом количества строк и сжатого размера на диске).

### 5️⃣ Генератор SDUI-форм и контекстные действия ([Stage 5/6])
- **`db.getConnectionFormSchema`**: генерация формы настройки подключения (`assets/connection_form.json`).
- **`sdui.contextActions` ([src/sdui/actions.rs](src/sdui/actions.rs))**: контекстное меню для аналитиков без написания DDL:
  - **Для таблиц (`table`)**: выборка *Top 100 Rows*, *Column Statistics* (быстрый профайлер), *Optimize Table (FINAL)*, *Deduplicate*, *Show DDL*.
  - **Для партиций (`partition`)**: *Drop Partition*, *Freeze Partition* (создание бэкапа/hardlink), *Detach Partition*.
  - **Для баз данных (`database`, `view`)**: мониторинг активных мутаций (`SYSTEM.mutations`) и процессов (`SYSTEM.processes`).

### 6️⃣ Отказоустойчивость, автовосстановление и Panic Hook ([Stage 6/6])
- **`std::panic::set_hook`**: перехват любых паник Rust, вывод диагностического отчета в `stderr`, зануление всех секретов в памяти (`zeroize`) и завершение с кодом **`101`** для корректного запуска экспоненциального backoff-перезапуска (`SandboxAutoRecovery`).
- Проверка и восстановление структуры scratch-директорий (`ensure_scratch_directories`) при каждом запуске драйвера.

---

## 🛠️ Сборка и тестирование

### Требования
- **Rust toolchain:** `stable` (edition 2024, Rust 1.85+)
- **OS:** Linux / macOS / Windows

### Команды сборки и проверки

```bash
# Проверка форматирования
cargo fmt --all -- --check

# Запуск строгого линтера
cargo clippy --all-targets --all-features -- -D warnings

# Запуск полного комплекта unit- и интеграционных тестов (48+ тестов)
cargo test --verbose --all

# Сборка релизного бинарного файла драйвера
cargo build --release
```

После успешной сборки исполняемый файл будет доступен по пути `target/release/clickhouse-query-ext`.

---

## 📋 Спецификация JSON-RPC методов

| Метод | Описание | Назначение |
| :--- | :--- | :--- |
| `system.handshake` | Обмен версиями и `capabilities` | Инициализация сессии Querya Host ↔ Rust |
| `system.ping` | Watchdog heartbeat | Быстрая проверка жизнеспособности (`< 5ms`) |
| `system.injectCredentials` | Передача пароля/JWT | Безопасное сохранение в In-Memory Pool |
| `system.shutdown` | Завершение работы | Очистка памяти и выход с кодом `0` |
| `db.connect` | Создание HTTP-клиента | Инициализация TLS и проверка соединения |
| `db.disconnect` | Закрытие сессии | Удаление клиента из глобального пула |
| `db.query` | Выборка строк (`SELECT`) | Возврат `RowCompact` с маппингом типов |
| `db.execute` | Выполнение DDL/DML | Возврат количества затронутых строк (`affectedRows`) |
| `db.cancelQuery` | Отмена запроса (`KILL QUERY`) | Остановка долгих вычислений по `query_id` |
| `db.getSchemaTree` | Список баз данных | Корневой уровень SDUI-дерева |
| `db.expandTreeNode` | Разворачивание узла | Подгрузка таблиц, вьюх, колонок и партиций |
| `db.getConnectionFormSchema`| Форма подключения | Отдача JSON-схемы настроек СУБД |
| `sdui.contextActions` | Контекстное меню | Генерация аналитических команд для UI |

---

## 👥 Структура репозитория

```text
clickhouse-query-ext/
├── assets/
│   ├── connection_form.json    # JSON-схема формы подключения
│   └── icon.svg                # Иконка расширения
├── docs/
│   ├── 01_TZ_RUST_ARCHITECTURE.md
│   └── 02_CLICKHOUSE_ANALYST_FEATURES.md
├── src/
│   ├── main.rs                 # Точка входа, инициализация Sandbox и асинхронный цикл
│   ├── error.rs                # Доменные ошибки DriverError и маппинг в коды JSON-RPC (-3260x)
│   ├── transport/              # Асинхронный NDJSON-транспорт (stdio.rs, framing.rs)
│   ├── rpc/                    # Роутер и обработчики JSON-RPC 2.0
│   ├── driver/                 # HTTP/TLS клиент, пул соединений, настройки сессий
│   ├── mapper/                 # Парсер типов ClickHouse и Row Format
│   ├── sdui/                   # Generative SDUI: дерево, формы и контекстные действия
│   └── utils/                  # Panic hook, recovery, zeroize секреты, санитазированный логгер
└── manifest.json               # Манифест расширения для Querya Desktop
```
