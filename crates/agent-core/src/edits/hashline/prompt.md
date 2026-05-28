# Hebbian Edit — Hashline Format

The `Edit` tool accepts a **hashline patch** string. This format lets you modify files by line number
instead of repeating the old text verbatim, saving tokens when editing large files.

## What `Read` returns

```
¶src/lib.rs#A1B
1:fn main() {
2:    println!("hi");
3:}
```

- First line `¶<path>#<HASH>` — path relative to workspace root; `HASH` is a 3-hex content fingerprint
- Body lines `N:<text>` — 1-based line numbers

## Patch syntax

**Required:** copy the `¶path#HASH` header exactly as `Read` gave it.

### Replace lines

```
¶src/lib.rs#A1B
2 2
+    println!("hello");
```

`2 2` means: replace original lines 2 through 2 (1-based, inclusive) with the lines below.

### Multiple hunks (same file)

```
¶a.rs#001
1 1
+// new top comment
5 7
+replaced block
```

Hunks must not overlap. They are applied back-to-front internally.

### Keep range — saves tokens

```
¶a.rs#001
1 20
+new first line
&3..15
+new last line
```

`&3..15` — keep original lines 3 through 15 verbatim. Use this instead of retyping unchanged code.

### Append to end of file

```
¶a.rs#001
EOF
+appended line A
+appended line B
```

### Multiple files in one patch

```
¶src/a.rs#001
1 1
+x

¶src/b.rs#FF2
3 4
+y
+z
```

## Rules

1. **Use the HASH from the most recent `Read` call.** If the file was modified since, the hash changed; you will get a `stale hash` error — re-read and retry.
2. **Line numbers come from the most recent `Read` output.** Do not guess.
3. **`+` lines contain the new content directly** — no line-number prefix.
4. Not supported in this backend: creating new files, deleting files, renaming.

## Error messages

- `stale hash` → file changed since last Read; call Read again, then Edit
- `out of range` → hunk line numbers exceed file length; check the Read output and correct them
