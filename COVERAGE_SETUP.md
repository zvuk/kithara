# Coverage Setup Instructions

## Overview

Инфраструктура для автоматического измерения code coverage настроена с использованием:
- **cargo-tarpaulin** - инструмент для измерения coverage в Rust
- **codecov.io** - сервис для отслеживания и визуализации coverage
- **GitHub Actions** - CI/CD для автоматического запуска coverage

---

## Файлы конфигурации

### 1. `tarpaulin.toml`
Конфигурация cargo-tarpaulin:
- Форматы вывода: HTML, XML (Cobertura), LCOV
- Исключения: `tests/`, `examples/`, `benches/`
- Timeout: 300 секунд
- Режим workspace

### 2. `codecov.yml`
Конфигурация Codecov:
- Целевой coverage: 80% (project), 70% (patches)
- Threshold: 2% для project, 5% для patches
- Flags: unit tests, integration tests
- GitHub Checks integration

### 3. `.github/workflows/coverage.yml`
GitHub Actions workflow:
- Запускается на push в main и на pull requests
- Устанавливает cargo-tarpaulin
- Генерирует coverage report
- Загружает в Codecov
- Сохраняет артефакты (30 дней)

---

## Настройка Codecov.io

### Шаг 1: Создание аккаунта

1. Перейти на https://codecov.io
2. Войти через GitHub account
3. Authorize Codecov app для доступа к репозиториям

### Шаг 2: Добавление репозитория

1. В Codecov dashboard: **Add new repository**
2. Выбрать `YOUR_ORG/kithara`
3. Скопировать **CODECOV_TOKEN**

### Шаг 3: Добавление секрета в GitHub

1. Перейти в Settings репозитория на GitHub
2. Secrets and variables → Actions
3. New repository secret
4. Name: `CODECOV_TOKEN`
5. Value: (вставить токен из Codecov)
6. Add secret

---

## Локальное измерение coverage

### Вариант 1: Docker (рекомендуется для macOS)

```bash
# Запустить tarpaulin в Docker контейнере
docker run --security-opt seccomp=unconfined \
  -v "${PWD}:/volume" \
  xd009642/tarpaulin:latest \
  cargo tarpaulin

# Результаты будут в target/coverage/
```

### Вариант 2: Нативная установка (Linux)

```bash
# Установить tarpaulin
cargo install cargo-tarpaulin

# Запустить coverage
cargo tarpaulin --workspace --out Html --output-dir ./coverage

# Открыть HTML отчет
open target/coverage/html/index.html
```

### Вариант 3: macOS (ограниченная поддержка)

⚠️ **Примечание:** tarpaulin имеет ограниченную поддержку на macOS.
Рекомендуется использовать Docker или GitHub Actions.

```bash
# Попробовать установить (может не работать)
cargo install cargo-tarpaulin

# Альтернатива: llvm-cov (сложнее настроить)
cargo install cargo-llvm-cov
cargo llvm-cov --workspace --html
```

---

## Первый запуск

### После настройки токена:

1. **Push изменений:**
   ```bash
   git push origin main
   ```

2. **Проверить workflow:**
   - Перейти в GitHub Actions tab
   - Найти workflow "Code Coverage"
   - Дождаться завершения (~5-10 минут)

3. **Проверить Codecov:**
   - Перейти на codecov.io/gh/YOUR_ORG/kithara
   - Увидеть baseline coverage percentage
   - Изучить coverage по файлам и функциям

---

## Ожидаемые результаты (оценка)

### Baseline Coverage (до mockall)

```
Overall coverage: ~65%

По модулям:
  kithara-net:       80% ✓  (хорошо протестирован)
  kithara-hls:       70% ⚠️  (много интеграционных тестов)
  kithara-decode:    75% ✓  (хорошие unit тесты)
  kithara-assets:    60% ⚠️  (нужны unit тесты для eviction)
  kithara-storage:   55% ❌ (мало unit тестов)
  kithara-stream:    65% ⚠️
  kithara-bufpool:   85% ✓
  kithara-worker:    70% ✓
  kithara-file:      60% ⚠️

Uncovered areas:
  - Error handling paths
  - Edge cases в ABR logic
  - Eviction policies
  - Crypto/encryption paths
```

### После Phase 1 (mockall + рефакторинг)

```
Target: 75-80%

Улучшения:
  - Изоляция unit-тестов (mockall)
  - Новые unit-тесты для storage
  - Новые unit-тесты для assets eviction
  - Property-based тесты для ABR
```

---

## Интерпретация результатов

### Coverage badges

После первого успешного запуска можно добавить badge в README.md:

```markdown
[![codecov](https://codecov.io/gh/YOUR_ORG/kithara/branch/main/graph/badge.svg)](https://codecov.io/gh/YOUR_ORG/kithara)
```

### Coverage trends

Codecov показывает:
- **Sunburst chart** - визуализация coverage по модулям
- **File tree** - coverage по каждому файлу
- **Commit history** - изменение coverage во времени
- **Pull request comments** - impact на coverage от каждого PR

### Threshold violations

Если coverage падает ниже threshold:
- GitHub Check будет **failed**
- Codecov добавит комментарий в PR
- Нужно добавить тесты для нового кода

---

## Troubleshooting

### Workflow fails с "CODECOV_TOKEN not found"

**Решение:** Проверить что токен добавлен в GitHub Secrets (см. "Настройка Codecov.io")

### Coverage показывает 0%

**Возможные причины:**
1. Тесты не запустились (проверить test logs)
2. Неправильный exclude pattern (проверить tarpaulin.toml)
3. XML файл не сгенерировался (проверить artifacts)

**Отладка:**
```bash
# Скачать artifacts из GitHub Actions
# Проверить coverage/cobertura.xml
```

### macOS: "tarpaulin не поддерживается"

**Решение:** Использовать Docker:
```bash
docker run --security-opt seccomp=unconfined \
  -v "${PWD}:/volume" \
  xd009642/tarpaulin:latest \
  cargo tarpaulin
```

---

## Следующие шаги

### Phase 2: Улучшение coverage

1. **Network isolation** (use MockNet):
   - Рефакторить HLS integration tests
   - Изолировать network calls
   - Ожидаемое улучшение: +5-10%

2. **Storage unit tests**:
   - Добавить edge cases для StreamingResource
   - Добавить error paths для AtomicResource
   - Ожидаемое улучшение: +10-15% для storage

3. **Assets eviction tests**:
   - Unit-тесты для LRU logic
   - Unit-тесты для pin/lease semantics
   - Ожидаемое улучшение: +10% для assets

4. **Property-based testing**:
   - ABR invariants (proptest)
   - Buffer pool invariants
   - Ожидаемое улучшение: +5%

### Целевые метрики

- **Week 1:** 65% baseline (Phase 1 complete)
- **Week 2:** 75% (Network isolation + tests)
- **Week 3:** 80% (Storage + Assets tests)
- **Week 4:** 85% (Property-based tests + edge cases)

---

## Ресурсы

**Tarpaulin:**
- GitHub: https://github.com/xd009642/tarpaulin
- Configuration: https://github.com/xd009642/tarpaulin#configuration

**Codecov:**
- Docs: https://docs.codecov.com
- Rust Guide: https://about.codecov.io/language/rust/
- YAML Reference: https://docs.codecov.com/docs/codecov-yaml

**GitHub Actions:**
- Codecov Action: https://github.com/codecov/codecov-action
- Workflow Syntax: https://docs.github.com/en/actions/using-workflows/workflow-syntax-for-github-actions
