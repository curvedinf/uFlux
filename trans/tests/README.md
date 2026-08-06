# trans/tests — coreutils adapted to the C subset

Each program is a hand-adapted version of the corresponding GNU coreutils
`src/<name>.c`, rewritten into the subset documented in `../README.md`.
All of them transpile with `../trans`, compile with `uf -c`, and their
behavior is diffed byte-for-byte against the system binaries.

## Adaptations vs GNU source

- **true.c / false.c** — GNU true/false handle `--help`/`--version`;
  here just `return 0;` / `return 1;` (arguments ignored, matching GNU
  behavior for ordinary args).
- **echo.c** — supports `-n`, `-e`, `-E` (combinable, leading args only,
  like GNU) and the escapes `\\ \a \b \c \f \n \r \t \v \0NNN \xHH`.
  Adaptations: character comparisons use numeric codes (the subset's string
  literals lack `\x`/`\0` escapes); the `-n`/`-e`/`-E` scan uses `__byte`-style
  `s[j]` indexing; no short-circuit `&&` across the argv boundary (the
  subset always evaluates both sides, so `argv[argc]` would be read).
  `--help`/`--version` are not special-cased.
- **yes.c** — prints the arguments joined by spaces (or `y`) forever via
  `printf("%s", ...)` / `puts`. Relies on SIGPIPE to terminate when piped,
  same as GNU.
- **wc.c** — default mode only (lines, words, bytes; no `-l/-w/-c/-m`
  flags, no wide chars). Reads stdin when no files are given; prints a
  `total` row for multiple files. Field width replicates GNU: digits of the
  summed byte counts across the file arguments (7 when reading stdin),
  printed with `%*d`. Because the subset has no division, the digit count
  is a comparison chain against powers of ten, and byte counts come from an
  `fgetc` pass (no `stat`). Error handling for unreadable files is omitted.

## Verified cases

- true/false: exit codes 0/1, with and without extra arguments.
- echo: plain words, `-n`, `-e` with `\t \n`, combined `-ne`, `-E`, `-n`
  alone, no args, octal `\061\062`, hex `\x41\x42`, `\c` truncation,
  escaped backslash.
- yes: default and with arguments, piped through `head`.
- wc: single file, multiple files + total row, 1-byte file (width 1),
  stdin (width 7) — all byte-identical to system `wc`.
