# Third-party notices

heyListen includes or adapts the following work. Models listed here are downloaded at build time, verified against a pinned SHA-256 (`build.rs`), and embedded in the binary.

## Echo cancellation (`src/aec.rs`)

**DTLN-aec model** (128-unit variant), by Nils L. Westhausen and Bernd T. Meyer: <https://github.com/breizhn/DTLN-aec>

> N. L. Westhausen and B. T. Meyer, "Acoustic echo cancellation with the dual-signal transformation LSTM network," ICASSP 2021.

**Processing code and the model's ONNX files**, adapted from Anarlog's `crates/aec`, by Fastrepl: <https://github.com/fastrepl/anarlog>. heyListen's version runs the model with `tract` instead of onnxruntime.

Both are under the MIT license:

```
MIT License

Copyright (c) 2020 Nils L. Westhausen
Copyright (c) 2023-present Fastrepl, Inc.

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

## Speaker recognition

- **pyannote segmentation 3.0** (MIT, CNRS), via sherpa-onnx: <https://huggingface.co/pyannote/segmentation-3.0>
- **3D-Speaker CAM++** speaker embeddings (Apache-2.0): <https://github.com/modelscope/3D-Speaker>

## Models downloaded by `heylisten setup`

These aren't part of the app; their licences apply when you download them:
- **NB-Whisper** (National Library of Norway): <https://huggingface.co/NbAiLab/nb-whisper-large>
- **Borealis** (National Library of Norway): <https://huggingface.co/NbAiLab/borealis-12b-gguf>
- **llama.cpp** (MIT): <https://github.com/ggml-org/llama.cpp>

Rust dependencies are listed in `Cargo.lock`, each under its own licence.
