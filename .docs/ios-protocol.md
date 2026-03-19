# iOS Audio Player Protocol

## AudioPlayerItemProtocol

```swift
protocol AudioPlayerItemProtocol {

    /// Для HLS ограничение битрейта сверху
    var preferredPeakBitRate: Double { get set }

    /// Для HLS ограничение битрейта сверху для мобильной сети
    var preferredPeakBitRateForExpensiveNetworks: Double { get set }

    /// Прогресс буферизации
    var bufferedDuration: Observable<Double?> { get }

    /// Длительность трека
    var duration: Observable<Double?> { get }

    /// Ошибка воспроизведения трека
    var error: Observable<Error> { get }

    /// Конструктор айтема
    /// - Parameters:
    ///   - url: URL на стрим
    ///   - additionalHeaders: дополнительные хедеры для запроса
    init(url: String, additionalHeaders: [AnyHashable: Any]?)

    /// Запускает загрузку стрима
    func load()
}
```

## AudioPlayerProtocol

```swift
protocol AudioPlayerProtocol {

    /// Громкость воспроизведения (0.0–1.0, clamped)
    var volume: Float { get set }

    /// Состояние mute
    var isMuted: Bool { get set }

    /// Скорость, на которой плеер должен воспроизводить трек
    var defaultRate: Float { get set }

    /// Текущий трек. Данный Observable возвращает текущий трек,
    /// когда для него прогружена длительность трека
    var currentItem: Observable<AudioPlayerItemProtocol> { get }

    /// Текущий прогресс воспроизведения, сек.
    /// Сбрасывается в 0 при удалении айтема из items
    var currentTime: Observable<Double> { get }

    /// Текущая скорость проигрывания.
    /// Если плеер что-то воспроизводит — `rate == defaultRate`,
    /// если плеер на паузе — `rate == 0`
    var rate: Observable<Float> { get }

    /// Ошибка воспроизведения плеера
    var error: Observable<Error> { get }

    /// Конструктор
    init()

    /// Массив треков, не путать с логической очередью плеера.
    /// Здесь обычно два-три трека: первый воспроизводится, второй прекешируется
    /// для плавного переключения и для возможности реализовать crossfade
    func items() -> [AudioPlayerItemProtocol]

    /// Вставка айтема в очередь.
    /// Если `afterItem == nil`, айтем добавляется в конец очереди (append).
    /// Это соответствует поведению AVPlayer.
    func insert(_ item: AudioPlayerItemProtocol, after afterItem: AudioPlayerItemProtocol?)

    func remove(_ item: AudioPlayerItemProtocol)

    func removeAllItems()

    /// Запуск воспроизведения
    func play()

    /// Пауза. Возобновление воспроизведения с прогресса `currentTime`
    func pause()

    /// Перемещение по прогрессу воспроизведения.
    /// - Parameters:
    ///   - to: позиция в секундах
    ///   - completionHandler: `true` — seek завершился успешно, иначе `false`
    func seek(to: Double, completionHandler: (Bool) -> Void)
}
```
