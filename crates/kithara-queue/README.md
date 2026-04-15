# kithara-queue

AVQueuePlayer-analogue orchestration layer on top of `kithara-play`. Owns the
queue (ordered tracks), async track loader with parallelism cap, navigation
(shuffle / repeat / history), and crossfade-aware track selection. Intended
to replace the bespoke queue / controller code duplicated in `kithara-app`,
`kithara-ffi`, and upcoming iOS / Android SDK surfaces.

## Overview

_TODO (filled in step C.8)._ Brief description of the Queue facade, the event
flow, and the relationship to `kithara-play::PlayerImpl`.

## Public API

_TODO (filled in step C.8)._ `Queue`, `QueueConfig`, `TrackSource`,
`TrackEntry`, `TrackStatus`, `TrackId`, `RepeatMode`, `QueueError`,
`QueueEvent`.

## Event Flow

_TODO (filled in step C.8)._ How `QueueEvent` is published on the shared event
bus and how subscribers receive unified queue + player + audio + hls events.

## Migration From kithara-app

_TODO (filled in step C.8)._ Mapping from `kithara-app::{playlist,
controls}` to `Queue` methods, including DRM handling that stays in the
caller.
