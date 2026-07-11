#!/usr/bin/env bash
set -euo pipefail

echo "Creating GitHub Issues for ClickHouse Query Extension (Rust Driver)..."

create_issue() {
  local title="$1"
  local body="$2"

  if gh issue list --state open --search "$title" | grep -q "$title"; then
    echo "✔ Issue '$title' already exists, skipping."
    return 0
  fi

  echo "Creating issue: '$title'..."
  local count=0
  until gh issue create --title "$title" --body "$body"; do
    count=$((count + 1))
    if [ "$count" -ge 5 ]; then
      echo "✖ Failed to create issue '$title' after 5 attempts."
      return 1
    fi
    echo "⚠️ Timeout/Error encountered. Retrying ($count/5) in 3 seconds..."
    sleep 3
  done
  sleep 1
}

# Issue 1
create_issue "[Stage 1/6] Инициализация Cargo workspace, структура модулей и manifest.json" "### 🎯 Цель
Создать базовый каркас Rust-проекта, настроить сборочные зависимости (\`Cargo.toml\`) и манифест расширения Querya (\`manifest.json\`).

### 📋 Описание задачи
1. Инициализировать \`Cargo.toml\` с необходимыми зависимостями: \`tokio\`, \`serde\`, \`serde_json\`, \`secrecy\`, \`tracing\`, \`tracing-subscriber\`, \`thiserror\`.
2. Создать модульную структуру директорий: \`src/{transport, rpc, driver, mapper, sdui, utils}/mod.rs\`.
3. Добавить файл \`manifest.json\` в корне каталога согласно спецификации Блока E (\`sandbox.engine: process\`, \`memory_mb: 256\`, \`network: connection_host_only\`, \`secretsStore: true\`).

### ✅ Критерий приемки (Definition of Done)
- [ ] Проект успешно собирается командой \`cargo check\`.
- [ ] Модули четко разделены и документированы.
- [ ] \`manifest.json\` содержит все обязательные поля по ТЗ (\`docs/01_TZ_RUST_ARCHITECTURE.md\`)."

# Issue 2
create_issue "[Stage 1/6] Реализация Stdio NDJSON транспорта и санитазированного логирования (Блок C)" "### 🎯 Цель
Обеспечить построчный асинхронный I/O (\`stdin/stdout\`) в формате \`Newline Delimited JSON (NDJSON)\` и безопасный вывод отладочных логов исключительно в \`stderr\`.

### 📋 Описание задачи
1. Реализовать модуль \`transport::stdio\`, читающий \`stdin\` построчно через \`tokio::io::BufReader::new(tokio::io::stdin()).lines()\`.
2. Реализовать потокобезопасную запись JSON-RPC ответов строго по одной строке в \`stdout\` (с немедленным \`flush()\`).
3. Настроить \`utils::logger\` (\`tracing-subscriber\`) для вывода отладочной информации только в \`io::stderr\`, чтобы не ломать парсер JSON-RPC в \`stdout\`.
4. Добавить санитайзер-фильтр для предотвращения попадания паролей/токенов в \`stderr\`.

### ✅ Критерий приемки (Definition of Done)
- [ ] Чтение из \`stdin\` и запись в \`stdout\` работают асинхронно без блокировок.
- [ ] Любые вызовы \`tracing::info!()\` / \`tracing::error!()\` идут только в \`stderr\`.
- [ ] Попытка залогировать строку с \`password=\` заменяется на \`[REDACTED BY RUST DRIVER]\`."

# Issue 3
create_issue "[Stage 1/6] Реализация системных методов JSON-RPC: handshake, ping и shutdown" "### 🎯 Цель
Поддержать обязательный системный контракт жизненного цикла с хостом Querya Desktop.

### 📋 Описание задачи
Реализовать обработчики системных методов в \`rpc::handlers::system\`:
1. **\`system.handshake\`**: проверка совместимости протокола (\`queryaVersion\`) и возврат списка поддерживаемых возможностей (\`capabilities\`). Должен отвечать менее чем за **3 секунды** после старта процесса.
2. **\`system.ping\`**: heartbeat для \`SandboxWatchdog\`. Должен мгновенно (за \`< 5ms\`) возвращать \`\"pong\"\`, даже если в этот момент выполняется тяжелый SQL-запрос (запуск пинга в независимом таске Tokio).
3. **\`system.shutdown\`**: корректное закрытие каналов I/O, сброс буферов и завершение процесса через \`std::process::exit(0)\`.

### ✅ Критерий приемки (Definition of Done)
- [ ] Драйвер корректно отвечает на \`system.handshake\` и \`system.ping\`.
- [ ] При получении \`system.shutdown\` процесс возвращает \`{\"ok\": true}\` и завершается с кодом \`0\`."

# Issue 4
create_issue "[Stage 2/6] Реализация in-memory хранилища секретов (system.injectCredentials)" "### 🎯 Цель
Безопасное получение и хранение паролей подключения в памяти без утечки через аргументы запуска (\`argv\`) и переменные окружения (\`env\`).

### 📋 Описание задачи
1. Реализовать обработчик метода \`system.injectCredentials\` (\`rpc::handlers::system\`).
2. Создать структуру \`ConnectionSecretsPool\` для хранения учетных данных в обертке \`secrecy::SecretString\` (\`zeroize\`).
3. Связать секрет с идентификатором подключения \`connectionId\`.
4. Гарантировать удаление страниц памяти с секретами при закрытии подключения или получении команды \`system.shutdown\`.

### ✅ Критерий приемки (Definition of Done)
- [ ] Секреты принимаются через JSON-RPC \`system.injectCredentials\`.
- [ ] Пароли не отображаются при логировании и удаляются из RAM при очистке."

# Issue 5
create_issue "[Stage 2/6] HTTP/HTTPS клиент ClickHouse и менеджер подключений (db.connect / db.disconnect)" "### 🎯 Цель
Организовать пул соединений с кластером ClickHouse через HTTP API (\`reqwest\`) с поддержкой SSL/TLS и проверкой работоспособности.

### 📋 Описание задачи
1. Создать обертку \`driver::client\` над \`reqwest::Client\` с настройками \`rustls-tls\` и поддержкой режимов SSL (\`disable\`, \`prefer\`, \`require\`).
2. Реализовать методы JSON-RPC \`db.connect\`, \`db.disconnect\` и \`db.testConnection\`.
3. При вызове \`db.connect\` выполнять тестовый запрос \`SELECT version(), timezone()\` для проверки авторизации и доступности сервера ClickHouse.
4. Хранить активные сессии в In-Memory реестре (\`driver::pool\`).

### ✅ Критерий приемки (Definition of Done)
- [ ] Успешное подключение к СУБД ClickHouse возвращает версию сервера и часовой пояс.
- [ ] Поддерживается HTTP и HTTPS подключение в рамках ограничения песочницы \`connection_host_only\`."

# Issue 6
create_issue "[Stage 3/6] Генерация схемы формы подключения (db.getConnectionFormSchema)" "### 🎯 Цель
Отдача валидной SDUI-схемы подключения для генерации интерфейса в Querya Desktop (\`SduiFormSchema\`).

### 📋 Описание задачи
1. Реализовать метод \`db.getConnectionFormSchema\` и создать статический резервный файл \`assets/connection_form.json\`.
2. Включить поля ввода: Хост, HTTP порт (по умолчанию 8123) / TCP порт (9000), Имя пользователя, Пароль (с пометкой об in-memory передаче).
3. Добавить селектор режима SSL (\`disable\`, \`prefer\`, \`require\`).
4. Добавить расширенные настройки ClickHouse для аналитиков: переключатель **Аналитический режим (Safe Mode / Read-Only)**, \`max_execution_time\`, \`join_use_nulls\`.

### ✅ Критерий приемки (Definition of Done)
- [ ] Метод \`db.getConnectionFormSchema\` возвращает JSON по спецификации SDUI Блока A.
- [ ] Схема проходит валидацию и содержит все параметры для аналитиков."

# Issue 7
create_issue "[Stage 3/6] Интроспекция баз данных, таблиц, представлений и словарей (db.getSchemaTree & expandTreeNode)" "### 🎯 Цель
Построение иерархического дерева обозревателя кластера ClickHouse (Lazy Tree) для боковой панели приложения.

### 📋 Описание задачи
1. **\`db.getSchemaTree\`**: выполнение запроса к \`SYSTEM.databases\` для получения списка баз данных и их движков (\`Ordinary\`, \`Atomic\`, \`Lazy\`).
2. **\`db.expandTreeNode\` для баз**: выполнение запроса к \`SYSTEM.tables\` с разделением объектов на:
   - Таблицы (\`MergeTree\`, \`Distributed\` и др.),
   - Представления (\`View\`, \`MaterializedView\`, \`LiveView\`),
   - Словари (\`SYSTEM.dictionaries\` с отображением статуса \`LOADED\`, количества элементов и использованной памяти).
3. Для **MaterializedView** указывать в метаданных связь со скрытой целевой таблицей \`.inner.id.<name>\`.

### ✅ Критерий приемки (Definition of Done)
- [ ] Дерево схемы точно отображает все типы объектов СУБД ClickHouse.
- [ ] В метаданных узлов передается количество строк, размер в байтах и комментарии."

# Issue 8
create_issue "[Stage 3/6] Интроспекция партиций и структуры таблиц (db.expandTreeNode для таблиц)" "### 🎯 Цель
Детализация структуры выбранной таблицы: список колонок и детализированная информация о партициях (\`SYSTEM.parts\`).

### 📋 Описание задачи
1. При расширении узла таблицы запрашивать список колонок и их типов из \`SYSTEM.columns\`.
2. Формировать дочерний раздел **\"Partitions (Партиции)\"**, выполняющий агрегированный запрос к \`SYSTEM.parts\`:
   \`\`\`sql
   SELECT partition, sum(rows) as total_rows, formatReadableSize(sum(data_compressed_bytes)) as compressed_size, count() as parts_count, min(min_time) as min_time, max(max_time) as max_time
   FROM system.parts WHERE database = {db} AND table = {table} AND active = 1 GROUP BY partition ORDER BY partition DESC
   \`\`\`
3. Отображать каждую активную партицию как отдельный узел с иконкой и информацией о размере и количестве строк.

### ✅ Критерий приемки (Definition of Done)
- [ ] Аналитик видит в дереве все колонки с их нативными типами ClickHouse.
- [ ] Раздел партиций показывает размер, число строк и временные границы каждой партиции."

# Issue 9
create_issue "[Stage 4/6] Маппинг типов данных ClickHouse в Querya Standard Schema (mapper::types)" "### 🎯 Цель
Точный и безопасный маппинг колоночных типов ClickHouse в стандартизованные типы UI без потери точности.

### 📋 Описание задачи
Реализовать конвертер типов в \`mapper::types\` согласно спецификации \`docs/02_CLICKHOUSE_ANALYST_FEATURES.md\`:
1. **Числа с фиксированной точкой и большие целые (\`Int64/UInt64/Int128/256\`, \`Decimal(P,S)\`)**: маппить в **строковый тип (\`string\`)** при передаче в \`rows\`, чтобы предотвратить обрезание или искажение больших чисел в JavaScript/Flutter \`Double\`.
2. **Даты и время (\`Date/Date32\`, \`DateTime/DateTime64\`)**: маппить в ISO 8601 UTC строки (\`YYYY-MM-DDTHH:mm:ss.sssZ\`).
3. **Вложенные типы (\`Array(T)\`, \`Tuple(...)\`, \`Map(K,V)\`)**: парсить и сериализовать в валидные JSON-массивы и объекты.
4. **Прозрачные обертки (\`Nullable(T)\`, \`LowCardinality(String)\`)**: корректно извлекать базовый тип и обрабатывать \`null\`.

### ✅ Критерий приемки (Definition of Done)
- [ ] \`UInt64\` / \`Decimal\` передаются без потери точности как строки.
- [ ] Все сложные и вложенные типы ClickHouse успешно конвертируются."

# Issue 10
create_issue "[Stage 4/6] Выполнение запросов (db.query) со Streaming-парсером и контролем RAM" "### 🎯 Цель
Высокоскоростное выполнение аналитических SQL-запросов в формате \`JSONCompactEachRowWithNamesAndTypes\` с потоковой обработкой и соблюдением квот песочницы.

### 📋 Описание задачи
1. Реализовать метод \`db.query\` / \`db.execute\`, отправляющий запрос в ClickHouse с HTTP заголовком \`X-ClickHouse-Format: JSONCompactEachRowWithNamesAndTypes\`.
2. Использовать асинхронное потоковое чтение байтов (\`reqwest::Response::bytes_stream()\`) для построчной десериализации JSON без полной буферизации тела ответа в оперативную память.
3. Формировать пачки строк (\`Chunks\` по 5 000 строк) с применением маппинга типов.
4. **Контроль RAM:** отслеживать потребление памяти процессом. Если память приближается к **200 МБ** (из 256 МБ доступных), корректно останавливать чтение потока и возвращать флаг \`isTruncated: true\`.

### ✅ Критерий приемки (Definition of Done)
- [ ] Запросы на 100 000+ строк выполняются без превышения лимита 256 МБ RAM.
- [ ] Возвращается точное время выполнения (\`executionTimeMs\`) и статистика прочитанных байт/строк (\`statistics\`)."

# Issue 11
create_issue "[Stage 4/6] Моментальная отмена выполняющихся запросов (db.cancelQuery)" "### 🎯 Цель
Предоставить аналитикам возможность моментально прерывать длительные или зависшие OLAP-запросы по кнопке \"Stop\" в интерфейсе.

### 📋 Описание задачи
1. При запуске любого запроса в \`db.query\` драйвер генерирует уникальный идентификатор и передает его в URL ClickHouse как параметр \`query_id=querya-job-<uuid>\`.
2. Реализовать обработчик метода \`db.cancelQuery\` (\`rpc::handlers::query\`).
3. При получении \`db.cancelQuery\` драйвер немедленно отправляет параллельный HTTP-запрос в ClickHouse:
   \`\`\`sql
   KILL QUERY WHERE query_id = 'querya-job-<uuid>' SYNC
   \`\`\`
4. Освободить ресурсы потока чтения в Tokio и вернуть подтверждение в UI.

### ✅ Критерий приемки (Definition of Done)
- [ ] Тяжелый запрос (\`SELECT count() FROM numbers(100000000000)\`) мгновенно останавливается на сервере ClickHouse при вызове \`db.cancelQuery\`."

# Issue 12
create_issue "[Stage 5/6] Контекстные меню и быстрые выборки для таблиц (sdui.contextActions)" "### 🎯 Цель
Добавить в боковую панель Querya быстрые аналитические команды по правой кнопке мыши на таблице.

### 📋 Описание задачи
Реализовать обработку команд контекстного меню в \`sdui::actions\`:
1. **Top 100 Rows**: мгновенное выполнение \`SELECT * FROM {db}.{table} LIMIT 100\`.
2. **Column Statistics (Быстрый профайлер)**: расчет базовой статистики для выбранной колонки таблицы:
   \`\`\`sql
   SELECT count() as total_rows, countIf(isNotNull({col})) as not_nulls, uniqExact({col}) as unique_exact, min({col}) as min_val, max({col}) as max_val, topK(5)({col}) as top_5_values FROM {db}.{table}
   \`\`\`
3. **Show DDL**: выполнение \`SHOW CREATE TABLE {db}.{table}\` для вывода точного скрипта создания со всеми настройками движка и гранулярности индексов.

### ✅ Критерий приемки (Definition of Done)
- [ ] По клику в меню таблицы моментально открывается вкладка с результатами выборки или статистики."

# Issue 13
create_issue "[Stage 5/6] Оптимизация таблиц и дедупликация (MergeTree Maintenance)" "### 🎯 Цель
Упростить обслуживание таблиц семейства MergeTree (\`ReplacingMergeTree\`, \`CollapsingMergeTree\`, \`SummingMergeTree\`) для аналитиков данных.

### 📋 Описание задачи
Добавить в контекстное меню и обработчик команды для принудительного слияния кусков данных:
1. **Optimize Table Final**: запуск команды \`OPTIMIZE TABLE {db}.{table} FINAL\` для слияния всех кусков в один и применения логики движка.
2. **Optimize Deduplicate**: запуск команды \`OPTIMIZE TABLE {db}.{table} DEDUPLICATE\` для очистки дублирующихся записей.
3. Отслеживать время выполнения оптимизации и возвращать подробный статус или ошибку в UI.

### ✅ Критерий приемки (Definition of Done)
- [ ] Команды \`OPTIMIZE ... FINAL / DEDUPLICATE\` успешно выполняются из контекстного меню с уведомлением пользователя."

# Issue 14
create_issue "[Stage 5/6] Управление жизненным циклом партиций: Drop, Freeze, Detach/Attach" "### 🎯 Цель
Обеспечить аналитикам удобный интерфейс управления дисковым пространством и историческими срезами данных через меню партиций.

### 📋 Описание задачи
Реализовать обработчики контекстных команд для конкретного узла партиции (\`partition\`):
1. **Freeze Partition (Мгновенный бэкап)**: выполнение \`ALTER TABLE {db}.{table} FREEZE PARTITION '{partition}'\` (создание hardlinks в \`/shadow/\`).
2. **Drop Partition**: удаление устаревшего среза данных \`ALTER TABLE {db}.{table} DROP PARTITION '{partition}'\` (обязательно запрашивать подтверждение в UI перед выполнением!).
3. **Detach / Attach Partition**: отсоединение партиции от таблицы и обратное прикрепление для архивации или переноса на S3.

### ✅ Критерий приемки (Definition of Done)
- [ ] Операции над партициями выполняются корректно для выбранного \`partition_id\`.
- [ ] Опасное удаление (\`DROP PARTITION\`) защищено подтверждением."

# Issue 15
create_issue "[Stage 5/6] Мониторинг активных процессов и мутаций (SYSTEM.processes & SYSTEM.mutations)" "### 🎯 Цель
Интроспекция текущей нагрузки и отслеживание фоновых изменений на кластере ClickHouse в 1 клик.

### 📋 Описание задачи
1. **Active Mutations**: запрос к \`SYSTEM.mutations\` для просмотра выполняющихся тяжелых операций \`ALTER TABLE ... UPDATE/DELETE\`:
   \`\`\`sql
   SELECT mutation_id, command, create_time, parts_to_do, is_done FROM system.mutations WHERE table = {table} AND is_done = 0
   \`\`\`
2. **Active Queries (\`SYSTEM.processes\`)**: просмотр списка активных запросов на сервере ClickHouse с указанием пользователя, потребляемой памяти и времени выполнения.
3. Предоставить кнопку **Kill Mutation / Kill Query** для аварийного завершения зависших процессов прямо из панели интроспекции.

### ✅ Критерий приемки (Definition of Done)
- [ ] Аналитик видит прогресс выполнения мутаций и может прервать блокирующий запрос на сервере."

# Issue 16
create_issue "[Stage 5/6] Аналитический Safe Mode (Режим Read-Only и защита от опасных DDL)" "### 🎯 Цель
Защитить продакшен-кластеры ClickHouse от случайного повреждения или перегрузки при аналитической работе.

### 📋 Описание задачи
Реализовать поддержку переключателя **Аналитический режим (Safe Mode / Read-Only)** в форме подключения:
1. **Настройки сессии на сервере:** при активном Safe Mode автоматически добавлять параметры \`readonly=1\`, \`max_execution_time=300\` (5 минут) и \`max_memory_usage=10000000000\` (10 ГБ) ко всем HTTP-запросам.
2. **Пре-фильтр AST на стороне Rust:** перед отправкой запроса на сервер проверять первые SQL-токены. Если запрос начинается с \`DROP DATABASE\`, \`TRUNCATE TABLE\` или \`ALTER TABLE ... DROP COLUMN\`, драйвер немедленно блокирует выполнение и возвращает ошибку \`-32603: Operation blocked by Safe Mode\`.

### ✅ Критерий приемки (Definition of Done)
- [ ] При включенном Safe Mode попытка выполнить \`DROP TABLE\` или тяжелый бесконечный запрос блокируется как на клиенте, так и на сервере ClickHouse."

# Issue 17
create_issue "[Stage 6/6] Unit-тесты маппинга типов, парсера протокола и безопасности памяти" "### 🎯 Цель
Обеспечить автоматическое тестовое покрытие критических компонентов Rust-драйвера.

### 📋 Описание задачи
1. Написать unit-тесты для \`mapper::types\` и \`mapper::row_compact\`: проверка корректной десериализации ответа \`JSONCompactEachRowWithNamesAndTypes\` для всех нативных типов (\`Int64\`, \`Decimal\`, \`DateTime64\`, \`Array\`, \`Tuple\`).
2. Написать асинхронные тесты для \`transport::framing\` и \`rpc::router\`: проверка обработки \`system.handshake\`, \`system.ping\` и корректного маппинга ошибок JSON-RPC.
3. Проверить зануление памяти в \`secrecy::SecretString\` после закрытия подключения.

### ✅ Критерий приемки (Definition of Done)
- [ ] Команда \`cargo test\` успешно выполняет 100% тестов.
- [ ] Отсутствуют паники или ошибки парсинга типов."

# Issue 18
create_issue "[Stage 6/6] Кросс-компиляция бинарников и сборка релизного .qext архива" "### 🎯 Цель
Подготовить готовые к дистрибуции артефакты расширения для загрузки в Querya Marketplace.

### 📋 Описание задачи
1. Настроить сборочный профиль \`[profile.release]\` в \`Cargo.toml\` (\`lto = true\`, \`strip = true\`, \`opt-level = 3\`, \`codegen-units = 1\`) для получения минимального размера исполняемого файла (~8-12 МБ).
2. Настроить скрипт сборки под 3 целевые платформы:
   - \`x86_64-unknown-linux-gnu\` (Linux),
   - \`aarch64-apple-darwin\` (macOS Apple Silicon),
   - \`x86_64-pc-windows-msvc\` (Windows).
3. Создать скрипт упаковки, объединяющий собранный бинарник, \`manifest.json\` и \`assets/\` в архив \`.qext\` (\`.zip\`) с подсчетом SHA-256 хеш-суммы для проверки безопасности в Блоке B.

### ✅ Критерий приемки (Definition of Done)
- [ ] Релизный бинарник собирается и весит менее 15 МБ.
- [ ] Архив \`.qext\` готов к установке через Менеджер расширений Querya."

echo "All 18 GitHub Issues processed successfully!"
