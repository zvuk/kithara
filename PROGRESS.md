# Progress

What is in flight right now. The
[GitHub Projects board](https://github.com/users/gerasim13/projects/3) owns
capability status and the roadmap, and git owns the facts. This file owns
intent: what is being worked on, what comes next, what is stuck. Update it in
the change that lands the work, and keep it short.

## In Flight

- Harness and document revision. `AGENTS.md` routes instead of restating, and
  the `style` namespace now budgets documents with `doc_size`, blocks drift with
  `doc_staleness`, and holds every crate README to one shape with `readme_shape`:
  a header that stays inside the package, badges keyed to `publish` and to the
  manifest's license, a `# <package name>` title, then `Usage` / `Key Types` /
  `Features` / `Integration` and nothing else. All three queues are at zero, and
  the rewrites turned up claims the sources contradict - a wrong feature list, a
  file that no longer exists, an inverted description of a known leak, an MPL-2.0
  crate wearing the MIT badge, two crates naming a dead owner, and a logo no
  published crate page could load.

- Full-playthrough queue census. A three-track queue is played from the first
  frame of the first track to the last frame of the last, and every output
  frame is attributed to the track that produced it. `PlayerTrack::render`
  carries a USDT probe naming the track, the block-relative span it was asked
  for, and the track's own media clock, so what a track contributed to a block
  is the clock's increase across it rather than the span it was handed. Both
  halves of a premature switch are pinned - a track must serve its whole
  length, and two tracks may share output frames only inside the crossfade the
  queue announced - and the rendered audio says the same thing twice more, by
  ramp provenance and by Cochlea. A one-line mutation that arms the crossfade a
  second early fails every leg on the serve-length assertion. The census runs
  over both readers a track can arrive through - HLS segments and a whole FLAC
  file named by `ResourceSrc::Path` - and over a queue that alternates between
  them at every seam, each at cf=0 and cf=1.0. The local legs read two new
  six-second ramp bodies from the fixture store, one per direction.

  The census now measures against the length its fixtures were built to rather
  than the duration the queue reports for them. The reported duration is what
  arms the crossfade, so a short report cut the track and shortened the
  expectation by the same amount; it is now a separately asserted property.
  HLS packages a segment as whole encoder frames, so its built length is the
  rounded figure - one owner, read by the packager and the census both.

  A third reader joined the census: the whole FLAC body served over HTTP as one
  range-capable response. HLS asks for a segment at a time and a local file is
  there in full, so neither ever reads past a download frontier; a whole body
  pulled over the network does, and that is the reader a playlist meets when it
  leaves a segmented stream for a file on a server. Keeping the FLAC ramp and
  changing only the transport is what lets every acoustic oracle keep working -
  the lossy-container problem below is a fixture problem, not a transport one.

  Two coverage holes found while hunting the premature switch, both now filled.
  Nothing played a streamed MPEG track to its end and checked the length that
  arrived, and nothing pinned the size-less MP3 read past the download boundary
  as a park rather than an end - the FLAC half of that pair had both.

  The hunt named a third, and it is the reported defect's shape. Every
  truncating delivery the test server offered advertises the full
  `Content-Length` first, so the client can always measure what arrived against
  a declared number. A `200` that names no total was missing, and that is the
  one case where a body that stops early is framed exactly like a complete one:
  the net layer reads the end as clean, the file layer commits the bytes it
  happened to write as the whole file, and a read past them answers `Eof`. The
  play layer then takes that `Eof` at face value - the trigger's EOF branch
  returns before it ever consults duration or position - so a track that lost
  its body two fifths in is announced as having played to its end, and the
  queue advances with a crossfade. `Delivery::UnsizedEarlyClose` serves that
  shape, and a test asks the reader to tell the two ends apart using the one
  number it still has: the length the track's own header names.

  The play layer now refuses that end. The reader announces how many frames are
  left once it has seen EOF, and that announcement is what arms the crossfade
  and what shrinks the visible duration onto itself - both a fade before the
  end, not at it, which is why a check at the end itself arrived a fade too
  late. An announcement pointing further from the declared length than the
  distance the triggers already use to call an end near is refused where it
  enters, so neither fires; the end that follows is reported as a failure, and
  the queue leaves the track the way it leaves a failed one - no crossfade. The
  distance now has one owner, shared by the triggers and the reader.

## Next

- A local MPEG leg for the census. Measured against six-second MP3 ramps: the
  probe half passes unchanged - each track serves its whole length and the seams
  overlap by exactly the configured crossfade - but two acoustic oracles cannot
  speak for a lossy container. The slope classifier sustains no class at a
  tolerance under 1.0, and 1.0 is where the two directions stop being told
  apart: at tol 2 a descending track reads as ascending, because the classes are
  only two units away and the ascending test is asked first. The take's sample
  peak is -4.74 dBFS against a single track's -6.02, so the lossy overshoot
  alone leaves the band that catches a fade summing instead of attenuating. The
  leg needs its own provenance instrument, not another fixture.
- Work the comment queue down by hand. `--fix` is exhausted for comments - a
  second run on a clean tree changes nothing - so all 668 are decisions: 497
  comments carrying prose outside a doc comment, 105 doc blocks past a dozen
  lines, 50 oversized inline comments, 16 dense functions. A body comment has no
  mechanical destination.
- 439 ordering findings are still mechanical: `struct_field_order` 160,
  `trait_item_order` 188, `struct_init_order` 91. One `just lint style --fix`
  clears them, but it rewrites declarations across every crate, so it wants its
  own change.
- Wire `just lint style` to a gate. Nothing runs it today - not the commit hook,
  not a CI lane - which is why the ratchet drifted unseen. A warm run is 58 s:
  too much for every commit, nothing for a lane. The lane catalog owns that
  change, so it does not belong in this one.

## Blocked

- Nothing.
