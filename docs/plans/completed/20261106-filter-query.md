Done: 2026-11-06, PR #123

# Filter queries (roadmap item 22)

The process filter stays what it is for plain text and gains `key:value` words, in the style of fsearch. Old filters keep working.

## Behavior

- A comma still separates terms: a row passes if it matches any term, and a term with only `!` hides what it matches. Each term keeps its total row, labeled with the lowercase text.
- Inside a term, words separated by spaces must all match: `node mem:>500m`. `!` before a word negates it: `node !user:root`.
- A plain word matches the name or pid, as today.
- Numbers take `>` or `<`, `>` when left out (`time:1h`): `cpu:>50`, `gpu:>5` (percent), `power:>1` (watts), `mem:>1g` (`k`, `m`, `g`, with or without `b`), `time:>1h` (`s`, `m`, `h`, `d`).
- Text is contained, case-insensitive: `user:root`, `path:/Applications`, `dir:code/macmon` (the working directory).
- A process with no value for a key doesn't match it: `power:>1` skips other users' processes, `dir:` skips what can't be read.
- A known key with a value that isn't complete yet (`mem:>`, `mem:>1x`) is ignored, so a filter being typed keeps its rows, as `safari,` does. An unknown key is plain text.

## Decisions

- No fuzzy matching, no parentheses, no `or` word: the comma is the OR.
- `app:` comes later, with roadmap item 16.
- No new dependency, no config change. Only the filter of the process list changes, not the one of the files list.

## Tasks

- [x] PR 1: words and keys, without `dir:`. Readme and help.
- [x] PR 2: `dir:` with the working directory of each process, measured for its cost.

## Result

The process filter takes words, `!` and the keys `cpu:`, `gpu:`, `power:`, `mem:`, `time:`, `user:`, `path:` and `dir:`. A sample while paused keeps the rows and updates only the totals. `dir:` reads the working directory only while a filter uses it, without a change to sampling: a refresh of about 970 processes took about 1.3 ms with `dir:` and 0.1 ms without.
