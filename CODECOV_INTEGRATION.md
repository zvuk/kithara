# Codecov Integration - Инструкция по настройке

## Обзор

Полная инструкция по интеграции Codecov.io для автоматического отслеживания code coverage в CI/CD.

---

## Шаг 1: Создание Codecov аккаунта

### 1.1. Регистрация

1. Перейти на https://codecov.io
2. Нажать **"Sign up with GitHub"**
3. Авторизовать Codecov app для доступа к вашим репозиториям

### 1.2. Добавление репозитория

1. После входа, в Codecov dashboard нажать **"Add new repository"**
2. Найти и выбрать репозиторий `kithara`
3. Codecov сгенерирует **CODECOV_TOKEN**
4. Скопировать токен (понадобится в шаге 2)

---

## Шаг 2: Добавление токена в GitHub

### 2.1. GitHub Secrets

1. Перейти в **Settings** репозитория на GitHub
2. В левом меню: **Secrets and variables** → **Actions**
3. Нажать **"New repository secret"**
4. Заполнить:
   - **Name:** `CODECOV_TOKEN`
   - **Secret:** (вставить токен из Codecov)
5. Нажать **"Add secret"**

### 2.2. Проверка

Токен должен появиться в списке secrets как `CODECOV_TOKEN` (значение скрыто).

---

## Шаг 3: Первый запуск

### 3.1. Push в GitHub

Coverage workflow уже настроен в `.github/workflows/coverage.yml` и запустится автоматически:

```bash
git push origin main
```

### 3.2. Мониторинг workflow

1. Перейти в **Actions** tab на GitHub
2. Найти workflow **"Code Coverage"**
3. Дождаться завершения (~5-10 минут)

**Что происходит:**
- Установка Rust stable
- Установка cargo-tarpaulin
- Компиляция workspace с инструментацией
- Запуск всех тестов
- Генерация coverage report (XML)
- Загрузка в Codecov
- Сохранение artifacts (HTML отчет)

### 3.3. Проверка результатов

После завершения:

1. **Codecov Dashboard:**
   - Перейти на https://codecov.io/gh/YOUR_ORG/kithara
   - Увидеть baseline coverage percentage
   - Изучить coverage по файлам и функциям

2. **GitHub Actions Artifacts:**
   - В завершенном workflow нажать **"coverage-report"**
   - Скачать и открыть `index.html` для просмотра HTML отчета

---

## Шаг 4: Добавление badge в README

После первого успешного запуска:

### 4.1. Получить badge URL

В Codecov dashboard для репозитория:
1. Нажать **"Settings"** → **"Badge"**
2. Скопировать markdown для badge

### 4.2. Добавить в README.md

```markdown
# Kithara

[![codecov](https://codecov.io/gh/YOUR_ORG/kithara/branch/main/graph/badge.svg)](https://codecov.io/gh/YOUR_ORG/kithara)

Audio streaming library for Rust...
```

---

## Настройки Coverage (codecov.yml)

Текущие настройки в `codecov.yml`:

```yaml
coverage:
  status:
    project:
      default:
        target: 80%      # Минимальный target coverage
        threshold: 2%    # Допустимое снижение перед failure

    patch:
      default:
        target: 70%      # Coverage для новых изменений
        threshold: 5%    # Более мягкий порог для patches
```

### Что означают настройки:

**Project Coverage (80% target):**
- Общий coverage всего проекта должен быть ≥ 80%
- Если coverage падает > 2% от базового, GitHub check будет **failed**
- Codecov добавит комментарий в PR

**Patch Coverage (70% target):**
- Новый код в PR должен иметь coverage ≥ 70%
- Если новый код покрыт < 70%, будет **warning**
- Более мягкий порог т.к. новый код может быть сложнее тестировать

**Threshold:**
- Project: -2% допустимо (79% ← 81% OK, 77% ← 81% FAIL)
- Patch: -5% допустимо (более гибко для новых feature)

---

## Ожидаемый Baseline Coverage

На основе анализа из Phase 1:

### Общий Coverage

```
Ожидаемый baseline: ~65-70%
```

### По модулям (прогноз)

```
Высокий coverage (75-85%):
  ✅ kithara-bufpool   ~85%  (хорошие unit тесты)
  ✅ kithara-net       ~80%  (много тестов)
  ✅ kithara-decode    ~75%  (unit + integration)

Средний coverage (65-75%):
  ⚠️  kithara-worker   ~70%  (unit тесты есть)
  ⚠️  kithara-hls      ~70%  (много integration, мало unit)
  ⚠️  kithara-stream   ~65%  (integration heavy)

Низкий coverage (50-65%):
  ❌ kithara-file      ~60%  (мало unit тестов)
  ❌ kithara-assets    ~60%  (eviction не покрыт)
  ❌ kithara-storage   ~55%  (edge cases не покрыты)
```

### Uncovered Areas (типичные)

- **Error handling paths** - часто не тестируются
- **Edge cases** - граничные условия
- **ABR logic** - сложные decision paths
- **Eviction policies** - LRU/TTL логика
- **Crypto paths** - encryption/decryption

---

## Улучшение Coverage (Post-Baseline)

После получения baseline, приоритеты:

### Phase 4: Quick Wins (70% → 75%)

**1. Storage unit tests (+10-15%):**
```rust
// Добавить unit тесты для StreamingResource edge cases
#[test]
fn test_write_at_beyond_eof() { ... }

#[test]
fn test_concurrent_reads_during_write() { ... }

#[test]
fn test_resource_failure_recovery() { ... }
```

**2. Assets eviction tests (+10%):**
```rust
// Unit тесты для LRU eviction logic
#[test]
fn test_lru_eviction_order() { ... }

#[test]
fn test_pin_prevents_eviction() { ... }

#[test]
fn test_lease_extends_lifetime() { ... }
```

**3. Error path coverage (+5%):**
- Добавить тесты с MockNet returning errors
- Timeout scenarios
- Network failures
- Invalid data handling

### Phase 5: Advanced (75% → 80%+)

**1. Property-based testing:**
```rust
use proptest::prelude::*;

proptest! {
    #[test]
    fn abr_never_exceeds_throughput(
        throughput in 100_000u64..10_000_000u64,
        buffer in 0.0f64..60.0f64
    ) {
        // ABR invariants verification
    }
}
```

**2. Integration → Unit migration:**
- Рефакторить integration тесты с TestServer
- Использовать MockNet для изоляции
- Быстрее + лучше coverage

**3. ABR edge cases:**
- Hysteresis logic
- Min interval enforcement
- Buffer-based decisions
- Variant selection edge cases

---

## Troubleshooting

### Problem: Workflow fails "CODECOV_TOKEN not found"

**Solution:**
1. Проверить что токен добавлен в GitHub Secrets
2. Имя должно быть точно `CODECOV_TOKEN` (case-sensitive)
3. Проверить что workflow имеет доступ к secrets

### Problem: Coverage показывает 0%

**Possible causes:**
1. Тесты не запустились (проверить test logs)
2. XML файл не сгенерировался (проверить artifacts)
3. Неправильный exclude pattern в tarpaulin.toml

**Debug:**
```bash
# Скачать artifacts из GitHub Actions
# Проверить coverage/cobertura.xml существует и не пустой
```

### Problem: Tarpaulin timeout

**Solution:**
- Увеличить timeout в tarpaulin.toml: `timeout = "600"`
- Или в workflow: `timeout: 600`

### Problem: macOS local testing

Tarpaulin имеет ограниченную поддержку на macOS.

**Solution 1: Docker (recommended):**
```bash
docker run --security-opt seccomp=unconfined \
  -v "${PWD}:/volume" \
  xd009642/tarpaulin:latest \
  cargo tarpaulin --workspace --timeout 300 --out Html
```

**Solution 2: llvm-cov (alternative):**
```bash
cargo install cargo-llvm-cov
cargo llvm-cov --workspace --html
```

---

## Мониторинг Coverage

### Codecov Dashboard Features

**1. Sunburst Chart:**
- Визуализация coverage по модулям
- Размер = размер кода, цвет = coverage %

**2. File Tree:**
- Coverage по каждому файлу
- Drill down в конкретные функции
- Показывает uncovered lines

**3. Commit History:**
- График изменения coverage во времени
- Видно когда coverage упал/вырос

**4. Pull Request Comments:**
- Автоматические комментарии в PR
- Diff coverage (какие новые линии не покрыты)
- Impact на общий coverage

### GitHub Checks Integration

При включенном `github_checks` в codecov.yml:

- ✅ **Success** - coverage выше target
- ⚠️ **Warning** - coverage в пределах threshold
- ❌ **Failure** - coverage значительно упал

---

## Best Practices

### 1. Maintain High Coverage

- **Target: 80%+** для production-ready кода
- Все новые features с тестами
- Покрывать error paths
- Unit > Integration (для coverage)

### 2. Review Coverage in PRs

Перед merge PR проверить:
- Codecov comment в PR
- Diff coverage (новый код покрыт?)
- Uncovered lines (критичные?)
- Overall impact (не упал ли общий coverage?)

### 3. Incremental Improvements

Не пытаться сразу 100%:
- Baseline → 70% (quick wins)
- 70% → 80% (systematic effort)
- 80%+ → 85% (diminishing returns)
- 100% часто не нужно (boilerplate, error handling)

### 4. Focus on Critical Code

Приоритизировать coverage для:
- ✅ Business logic (ABR, eviction)
- ✅ Error handling paths
- ✅ Edge cases и граничные условия
- ⬜ Boilerplate (getters, setters)
- ⬜ Очевидный код

---

## Следующие шаги после интеграции

### Immediate (после baseline)

1. ✅ Codecov account настроен
2. ✅ CODECOV_TOKEN добавлен
3. ✅ Workflow успешно запущен
4. ✅ Baseline coverage измерен
5. ✅ Badge добавлен в README

### Short-term (1-2 недели)

1. Проанализировать uncovered areas
2. Приоритизировать quick wins
3. Добавить unit тесты для storage
4. Добавить unit тесты для assets eviction
5. Target: достичь 70-75%

### Long-term (1 месяц)

1. Property-based тесты для ABR
2. Миграция integration → unit где возможно
3. Edge cases coverage
4. Target: достичь 80%+

---

## Ресурсы

**Codecov Documentation:**
- Docs: https://docs.codecov.com
- Rust Guide: https://about.codecov.io/language/rust/
- YAML Reference: https://docs.codecov.com/docs/codecov-yaml

**Tarpaulin:**
- GitHub: https://github.com/xd009642/tarpaulin
- Configuration: https://github.com/xd009642/tarpaulin#configuration

**GitHub Actions:**
- Codecov Action: https://github.com/codecov/codecov-action
- Workflow Syntax: https://docs.github.com/en/actions/using-workflows

---

## Summary Checklist

- [ ] Codecov аккаунт создан
- [ ] Репозиторий добавлен в Codecov
- [ ] CODECOV_TOKEN скопирован
- [ ] Токен добавлен в GitHub Secrets
- [ ] Push в main для запуска workflow
- [ ] Workflow успешно завершен
- [ ] Baseline coverage измерен
- [ ] Coverage badge добавлен в README
- [ ] Uncovered areas проанализированы
- [ ] План улучшения составлен

**Phase 3 Status: После выполнения всех шагов ✅**
