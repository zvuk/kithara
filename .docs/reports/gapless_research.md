Сейчас корректнее формулировать так: gapless в kithara частично делегирован декодерам/backend’ам, но не является собственной моделью движка. Это опасно: для MP3/Ogg/FLAC часть случаев может уже работать через Symphonia, а вот AAC/MP4/M4A почти наверняка требует отдельной обработки atoms/metadata, иначе будут слышны encoder delay в начале и padding в конце.

Что в kithara уже есть

kithara — модульный Rust audio engine со стримингом HTTP/HLS, cache/offline, crossfade/BPM sync и несколькими decode backend’ами: Symphonia и Apple AudioToolbox.  ￼

В kithara-decode уже есть флаг DecoderConfig.gapless, причём он включён по умолчанию и передаётся в Symphonia backend.  ￼ Но в Symphonia backend сейчас виден важный нюанс: DecoderOptions.gapless выставляется, а FormatOptions создаётся как FormatOptions::default(). По публичной документации Symphonia gapless нужно включать и на уровне format layer через FormatOptions::enable_gapless = true; без этого демультиплексор может не отдавать правильные packet trim/metadata даже если decoder умеет gapless. Это надо проверить именно на используемой версии symphonia = 0.6.0-alpha.1, но по текущему коду это первое место для ревизии.  ￼

Также у kithara есть Apple backend. Там уже читается AudioConverterPrimeInfo, то есть leading_frames и trailing_frames, через kAudioConverterPrimeInfo. Это хороший сигнал: часть информации уже можно поднять в общую модель. Но по видимому коду это скорее сохраняется/логируется как prime_info; не видно общей политики, которая гарантированно обрезает PCM на выходе и синхронизирует таймлайн.  ￼

Главный архитектурный пробел: PcmMeta содержит spec, frame offset, timestamp, segment/variant/epoch, но не содержит явного encoder_delay, encoder_padding, valid_frames, gapless_source или признака “уже применено backend’ом”.  ￼ Поэтому сейчас сложно гарантировать одинаковое поведение Symphonia, Apple, Android/FFmpeg и будущих backend’ов.

Что именно надо поддержать

Gapless playback — это не “убрать тишину”. Для lossy codecs это обычно точное удаление:

1. encoder delay / priming / leading frames — лишние samples в начале;
2. encoder padding / trailing frames — лишние samples в конце;
3. иногда ещё точная оригинальная длина.

Для AAC это особенно важно: encoder добавляет начальное заполнение из-за оконного MDCT, а decoder на конце выдаёт целые frames, поэтому появляются лишние samples. Корректный playback должен знать startup delay и original length / padding, удалить лишние samples и сшить треки без разрыва. Fraunhofer прямо описывает два источника проблемы и два вида metadata: стандартный MP4 Edit List и Apple/iTunes proprietary atoms.  ￼

Форматы и где они кодируют gapless

Формат / контейнер	Где лежит информация	Что делать в kithara
MP3	Xing/Info/LAME/Lavf header; иногда ID3 iTunSMPB	Парсить exact metadata. LAME/Xing хранит 24-bit value: верхние 12 бит — delay, нижние 12 бит — padding. iTunSMPB тоже может дать front padding, end padding и total non-padding sample count.  ￼
AAC in MP4/M4A	edts/elst edit list; Apple iTunSMPB; иногда backend prime info	Нужен MP4 atom parser. elst — предпочтительный стандартный путь. iTunSMPB — важный fallback для файлов из iTunes/afconvert.  ￼
AAC ADTS/raw	Надёжного стандартного места обычно нет	Exact gapless из одного ADTS-файла обычно невозможен без внешней metadata/backend info. Эвристику держать отдельно и считать low-confidence.
Ogg Opus	OpusHead.pre-skip + Ogg page granule position / EOS granule	Применять pre-skip в начале и EOS granule для end trim. RFC 7845 прямо задаёт pre-skip и расчёт PCM sample position через granule_position - pre_skip.  ￼
Opus in MP4	dOps.PreSkip, но фактическое trimming должно задаваться edit list; seek требует roll/pre-roll groups	Поддержать dOps как metadata, но для playback trimming ориентироваться на elst; sgpd/sbgp с roll важны для seek/pre-roll.  ￼
Ogg Vorbis	Ogg granule position	Начало/конец задаются granule positions: final page может указывать, что часть samples из последнего packet надо отбросить.  ￼
FLAC	Обычно lossy encoder delay нет; точная длина в STREAMINFO	Gapless в основном вопрос scheduler’а/буферизации между треками. STREAMINFO содержит total samples, если известно.  ￼
WAV/PCM	Нет codec padding; длина из контейнера	Ничего trim’ить не надо, но нужно не вставлять паузу между источниками.
HLS/fMP4/CMAF	Зависит от init segment/edit lists/timeline; внутри сегментов тримминг делать опасно	Не обрезать каждый media segment как отдельный трек. Gapless trim должен применяться к media item / rendition boundary, а не к каждому HLS chunk.

Для Symphonia важно: по публичной таблице поддержки gapless есть для MP3, FLAC, Vorbis, WavPack/WAV, но нет для ISO/MP4 и AAC-LC. Там же сказано, что gapless требует поддержки и со стороны demuxer, и со стороны decoder.  ￼ Поэтому “включить decoder_opts.gapless” не решает AAC-in-MP4/M4A.

MP4/M4A: что именно парсить

Для AAC/M4A основные источники такие.

Первый — edts/elst edit list. Android MP4Extractor использует edit list для gapless: если media_time > 0, это трактуется как leading delay, пересчитанный из track timescale в sample rate. Padding вычисляется из segment duration и общего количества decoded samples. В AOSP это затем кладётся в AMEDIAFORMAT_KEY_ENCODER_DELAY и AMEDIAFORMAT_KEY_ENCODER_PADDING.  ￼

Формула для kithara примерно такая:

leading_frames = media_time * sample_rate / media_timescale
valid_frames = segment_duration * sample_rate / movie_timescale
trailing_frames = decoded_total_frames - leading_frames - valid_frames

Нужно аккуратно работать с rounding. Для AAC sample rate обычно 44100/48000, timescale может совпадать с sample rate, но не обязан. Нельзя делать float-округление “на глаз”; лучше integer rational conversion с проверкой остатка.

Второй — Apple/iTunes iTunSMPB. Для MP4/M4A это metadata atom, для MP3 — ID3 tag. Из него обычно берутся hex-поля: первое игнорируется, дальше идут front padding, end padding и total non-padding sample count. Chrome/Web Audio в своё время документировал именно такую схему; ExoPlayer/Media3 тоже парсит com.apple.iTunes + iTunSMPB regex’ом и вытаскивает delay/padding из hex-полей.  ￼

Третий — backend-specific prime info. На Apple backend это AudioConverterPrimeInfo. На Android, если используется platform extractor/MediaCodec path, официальные MediaFormat.KEY_ENCODER_DELAY и KEY_ENCODER_PADDING появились в API 30 и означают количество frames, которые надо обрезать в начале и конце decoded audio stream.  ￼

Моя рекомендация по приоритету источников:

1. Explicit standard container metadata:
   MP4 elst / Opus granule / Vorbis granule / FLAC total samples
2. Well-known encoder metadata:
   iTunSMPB, Xing/LAME/Lavf
3. Platform/backend metadata:
   AudioConverterPrimeInfo, Android KEY_ENCODER_DELAY/PADDING
4. Heuristic silence trim:
   only opt-in / diagnostics / low-confidence

При конфликте источников не надо молча “среднее брать”. Лучше хранить source и confidence, логировать конфликт и иметь deterministic priority.

Предлагаемая модель в kithara

Я бы добавил маленькую внутреннюю модель, не завязанную на конкретный backend:

struct GaplessInfo {
leading_frames: u64,
trailing_frames: u64,
valid_frames: Option<u64>,
sample_rate: u32,
source: GaplessSource,
confidence: GaplessConfidence,
already_applied: bool,
}

Где GaplessSource примерно:

Mp4EditList
ITunSmpb
Mp3XingLame
OggOpusGranule
OggVorbisGranule
FlacStreamInfo
ApplePrimeInfo
AndroidMediaFormat
Heuristic
None

И отдельно нужен GaplessTrimmer над любым InnerDecoder:

decoder -> decoded PCM chunks -> GaplessTrimmer -> pipeline

Он должен делать две вещи:

В начале — отбросить leading_frames, даже если они пересекают несколько chunks.

В конце — удерживать последние trailing_frames в ring buffer и сбросить их только после EOF. Это единственный надёжный способ trim’ить хвост в streaming decoder, когда заранее не знаешь, где последний chunk.

Важно: trailing_frames — это frames per channel, не samples в смысле “float values”. Для stereo 1024 frames — это 2048 scalar samples. Все расчёты должны быть в frames.

Ещё важнее: нужен флаг already_applied. Если Symphonia уже вернула trimmed PCM для MP3/Vorbis/FLAC, внешний trimmer не должен резать второй раз. Это критично для файлов, где metadata одновременно видит и demuxer, и ваш parser.

Где использовать re_mp4

В workspace уже есть re_mp4 = "0.4.0", symphonia = "0.6.0-alpha.1" и ffmpeg-next = "8.1.0".  ￼ Поэтому для MP4 atoms логично не писать всё с нуля, а сначала проверить, можно ли через re_mp4 достать:

moov/trak/mdia/mdhd      -> media timescale
moov/mvhd               -> movie timescale
moov/trak/edts/elst     -> media_time, segment_duration
moov/trak/mdia/minf/stbl/stsd -> codec/sample entry, sample_rate
moov/udta/meta/ilst/... -> iTunSMPB / com.apple.iTunes
sgpd/sbgp roll          -> Opus pre-roll / seek correctness, если понадобится

Если re_mp4 не отдаёт iTunSMPB удобно, достаточно сделать узкий atom scanner только для metadata path. Не надо full MP4 parser ради одного atom, но elst лучше читать нормальным parser’ом, потому что там version 0/1, signed media_time, timescale conversion и edge cases.

Стратегия для эвристики

Эвристика silence trim — это не gapless, а аварийный режим. Её нельзя включать как основной путь.

Почему: реальный трек может намеренно начинаться с тишины, fade-in, room tone, паузы перед downbeat. Если эвристика срежет это, playback станет “чище”, но невернее. Для музыкального сервиса это особенно неприятно: пользователь услышит изменённый master.

Эвристика допустима как:

gapless_mode = ExactOnly        // default for production
gapless_mode = ExactThenHeuristic
gapless_mode = Disabled

И в GaplessInfo указывать confidence = Low.

Как протестировать, что сейчас gapless не обрабатывается

Тут нужны не только “послушал ушами”, а offline PCM tests.

1. Parser unit tests

Минимум:

iTunSMPB:
"00000000 00000840 000001F4 00000000003A1B2C ..."
-> delay = 0x840, padding = 0x1F4, total = 0x3A1B2C
MP3 Xing/LAME:
value = 0x8401F4
-> delay = value >> 12
-> padding = value & 0x0FFF
MP4 elst:
media_time = 2112
media_timescale = 44100
segment_duration = N
movie_timescale = 1000 or 44100
-> exact frame conversion
Invalid:
zero delay/padding
missing sample rate
negative media_time / empty edit
multiple edit list entries
conflicting elst + iTunSMPB

Для MP4 edit list стоит повторить подход Android: single edit list entry с positive media_time — это gapless case; empty edit обрабатывать отдельно, а сложные edit lists не интерпретировать как gapless без явной поддержки timeline editing.  ￼

2. Trimmer unit tests

Нужны synthetic PCM chunks, не настоящие файлы:

leading trim меньше первого chunk
leading trim больше первого chunk
trailing trim меньше последнего chunk
trailing trim больше последнего chunk
leading + trailing пересекают несколько chunks
mono/stereo
EOF без samples
seek/reset

Критерий: emitted frame count и содержимое samples ровно совпадают с ожидаемым slice.

3. Golden audio integration tests

Лучший готовый набор для AAC — Fraunhofer AAC Gapless Playback Test. Там два отдельно закодированных AAC-LC sweep-сегмента, и при правильном playback они должны сшиваться в непрерывный sweep без interruption/transients.  ￼

Набор файлов я бы сделал такой:

AAC/M4A with elst
AAC/M4A with iTunSMPB
AAC/M4A with both elst and iTunSMPB
MP3 with Xing/LAME
MP3 with iTunSMPB
Ogg Opus
Ogg Vorbis
FLAC/WAV negative controls
AAC ADTS without metadata negative control

Для каждого файла тест должен декодировать через kithara в offline mode, без output device, без crossfade и желательно без resampling.

4. Как увидеть текущую проблему

Берёте два adjacent tracks, которые были нарезаны из одного continuous source и отдельно закодированы. Дальше:

decode(track_1) -> pcm_1
decode(track_2) -> pcm_2
joined = pcm_1 + pcm_2

Проверяете boundary в окне, например ±4096 frames вокруг join.

Признаки отсутствия gapless:

extra near-zero run at boundary
sharp transient/click at boundary
cross-correlation показывает sample offset
total emitted frames больше expected valid frames
первый non-silent content сдвинут на encoder delay

Для AAC-LC типичные числа часто около 1024/2112 priming frames, но не надо хардкодить их как универсальные. Доверять нужно metadata.

Критерии успеха реализации

Я бы зафиксировал такие acceptance criteria.

Первое: точная длина. Для файлов с exact metadata:

emitted_frames == valid_frames

Если есть leading_frames и trailing_frames, то:

decoded_total_frames - emitted_frames == leading_frames + trailing_frames

Второе: нет артефакта на стыке. Для golden pair:

join(pcm_1_trimmed, pcm_2_trimmed)

не должен иметь дополнительной тишины, выброса или sample discontinuity относительно reference. Для lossy codecs лучше сравнивать не с исходным WAV напрямую, а с reference decode/known-good player output либо с самим тестовым критерием sweep continuity.

Третье: seek correctness. Seek to 0 не должен отдавать priming samples. Seek near end не должен отдавать padding. Для Opus/MP4 отдельно нужен pre-roll/roll handling, иначе seek может быть формально gapless на старте, но грязным после random access.

Четвёртое: backend parity. Один и тот же файл через Symphonia, Apple и Android/platform path должен давать одинаковый logical frame count и одинаковую позицию первого/последнего валидного frame. Допустимы малые codec/backend differences в значениях PCM, но не в количестве frames и не в таймлайне.

Пятое: никакого double trim. Файл с metadata, которую уже применил decoder, не должен быть обрезан повторно внешним GaplessTrimmer.

Шестое: negative controls не портятся. WAV/FLAC без lossy padding, AAC ADTS без metadata и треки с настоящей тишиной в начале/конце не должны самопроизвольно обрезаться при ExactOnly.

Седьмое: duration/UI соответствует logical audio, а не padded decoded stream. Если трек реально 180.000 секунд, UI/progress не должен показывать 180.048 из-за encoder padding.

Что бы я делал по этапам

Этап 1: ввести GaplessInfo и GaplessTrimmer, покрыть trimmer unit-тестами. Одновременно проверить Symphonia path: если в 0.6 alpha всё ещё нужен FormatOptions::enable_gapless, включить его вместе с DecoderOptions.gapless.

Этап 2: MP3. Добавить parser для Xing/LAME/Lavf gapless bits и iTunSMPB. Это относительно дешёвый и хорошо документированный слой; ExoPlayer/Media3 поведение можно использовать как reference для edge cases.  ￼

Этап 3: MP4/M4A. Через re_mp4 или узкий atom scanner достать elst и iTunSMPB. Для AAC/M4A это главный production-кейс.

Этап 4: Apple/Android backend propagation. Apple AudioConverterPrimeInfo и Android KEY_ENCODER_DELAY/PADDING должны заполнять тот же GaplessInfo, а не жить отдельно в backend-specific коде.  ￼

Этап 5: Opus/Vorbis/FLAC validation. Если Symphonia уже делает это корректно, оставить внешний parser только для диагностики/metadata reporting. Если нет — применить общий trimmer.

Этап 6: heuristic mode, но только как opt-in. Его стоит использовать для диагностики “похоже на padding”, но не как production default.

Главная мысль: gapless должен стать частью timeline contract kithara, а не побочным эффектом decoder backend’а. Тогда atoms, headers, platform metadata и эвристика сводятся в одну понятную модель, а тесты проверяют не “какой backend угадал”, а единое поведение движка.