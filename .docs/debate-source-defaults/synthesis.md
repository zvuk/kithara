# Debate Synthesis: Default Methods in Source and StreamType Traits

## Участники
- **Codex** (GPT) — round-1-codex.md
- **Gemini** — round-1-gemini.md
- **Claude** — round-1-claude.md

## Вопрос
Могут ли часть функций в имплементациях `Source` и `StreamType` быть default методами в самих трейтах, чтобы они были написаны один раз и более чётко очерчивали работу абстракций?

---

## Консенсус (все 3 участника согласны)

### 1. `StreamType::build_stream_context()` → default `NullStreamContext`

**Единогласно.** 4 из 5 имплементаций возвращают `NullStreamContext`. Только `Hls` переопределяет с `HlsStreamContext`.

```rust
fn build_stream_context(_source: &Self::Source, timeline: Timeline) -> Arc<dyn StreamContext> {
    Arc::new(NullStreamContext::new(timeline))
}
```

**Выгода**: убирает boilerplate из `File`, `MemStream`, `UnknownLenStream`, `DummyType`.

### 2. Удалить избыточный `MemorySource::media_info()` override

**Единогласно.** `MemorySource::media_info()` возвращает `None` — идентично дефолту трейта. Override не нужен.

### 3. НЕ добавлять дефолты для `wait_range()`, `read_at()`, `phase()`, `create()`

**Единогласно.** Это ядро контракта. Каждая имплементация фундаментально различна. Дефолт был бы либо бесполезен, либо опасен.

### 4. Объединить `MemorySource` + `UnknownLenSource`

**Единогласно** по сути, расхождение в реализации:
- Codex: общая структура с общими методами
- Gemini: `BufferSource<L: LengthPolicy>` (generic по типу)
- Claude: `report_len: bool` (простой флаг)

**Рекомендация**: `report_len: bool` — простейшее решение для 2 тестовых типов. Type-level generic оправдан только если появятся новые варианты.

---

## Частичный консенсус (2 из 3)

### 5. Улучшить `is_range_ready()` default

- **Codex**: делегировать к `phase()` — `matches!(self.phase(range), SourcePhase::Ready)`
- **Gemini**: `range.is_empty() || matches!(self.phase(range), SourcePhase::Ready)`
- **Claude**: только `range.is_empty()`, НЕ делегировать к `phase()`

**Проблема с делегацией к `phase()`**: `phase()` может вызывать `is_range_ready()` внутри (circular dependency). В текущей реализации `HlsSource::phase()` вызывает `range_ready_from_segments()` напрямую, но контракт трейта не запрещает циклическую зависимость.

**Рекомендация**: `range.is_empty()` — безопасный минимум. Кто хочет больше — переопределяет.

---

## Расхождения

### 6. `phase()` default

- **Codex и Gemini**: предлагают дефолт для упрощения тестовых источников
- **Claude**: против — дефолт пропускает `Eof`, `Seeking`, `Cancelled`, `Stopped` и вводит в заблуждение новых имплементоров

**Рекомендация**: НЕ добавлять. `phase()` — критический метод, каждый имплементор должен явно подумать о всех состояниях. Тестовые источники — не основание для ослабления контракта.

---

## Итоговые рекомендации (приоритет)

| # | Изменение | Консенсус | Усилия | Риск |
|---|-----------|-----------|--------|------|
| 1 | Default `build_stream_context` → `NullStreamContext` | 3/3 | Низкие | Нет |
| 2 | Удалить `MemorySource::media_info()` override | 3/3 | Минимальные | Нет |
| 3 | Объединить `MemorySource` + `UnknownLenSource` | 3/3 | Средние | Низкий |
| 4 | `is_range_ready` default: `range.is_empty()` | 2/3 | Минимальные | Нет |
| 5 | НЕ добавлять default для `phase()` | 2/3 | — | — |

## Ключевой вывод

Трейт `Source` уже хорошо спроектирован. 4 required метода правильно заставляют каждую имплементацию продумать свою модель доступности данных. 11 optional методов с no-op дефолтами корректно маркируют сегментные расширения как opt-in. Основная возможность для улучшения — `StreamType::build_stream_context()` и cleanup тестовых источников.

**Трейты определяют контракт, а не удобство.** Добавление дефолтов ради экономии 5-10 строк в тестовом коде ценой размытия контракта — плохой trade-off.
