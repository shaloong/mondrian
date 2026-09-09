# Blackmagic DeckLink API 12.0 interface provenance

The 27 files in `decklink-api-12.0` are unmodified Blackmagic Design interface
sources redistributed by the official OBS Studio repository, fixed to commit
`671fb57daf4972fcd506689a48a474dd4eda9e66` (tag `32.0.0`). The annotated tag object
`aab5d99c46afe317fff4b46c974b9ea8f1ed0b23` is not the source commit.

Source: https://github.com/obsproject/obs-studio/tree/671fb57daf4972fcd506689a48a474dd4eda9e66/plugins/decklink/win/decklink-sdk

Each file retains its complete Blackmagic copyright, permission grant,
notice-preservation requirement and disclaimer (Boost Software License 1.0
text). `sha256.json` records every original file's SHA-256 and byte length.
No OBS implementation source or GPL implementation code is copied here.
Microsoft MIDL generates the Windows COM declarations/IIDs at build time.
The SDK version header explicitly declares `12.0`, `0x0c000000`.

These interfaces are not DeckLink SDK 16. No SDK 16 registration, account,
license acceptance, runtime driver or hardware qualification is implied.
The actual installed Desktop Video COM runtime version is discovered separately.
