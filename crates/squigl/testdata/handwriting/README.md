Handwriting samples for the `glmocr_handwriting` test in `src/local/glmocr.rs`,
which checks that GLM-OCR still reads them the same way after an ONNX Runtime
(`ort`) bump:

```
cargo test -p squigl --release glmocr_handwriting -- --ignored --nocapture
```

- `NAME.png` -- a crop as the window saves it (Save PNG, ctrl+S). Read with the
  crop prompt; name it `formula-NAME.png` for the formula prompt, `page-NAME.png`
  for the page prompt.
- `NAME.webgpu.txt`, `NAME.cpu.txt` -- the reading recorded on each device. Not
  the true transcription: what the model said, so a change shows up as a diff.
  `SQUIGL_BLESS=1` (re)records them from a run.

The repository is public: only add handwriting that may be published.
