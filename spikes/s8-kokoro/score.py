# How well a listener (Whisper base.en, as a stand-in) understands each voice: the
# word error rate of its transcript against the sentence it was given, over the
# sentences of plain text (the maths ones are long and their words odd for ASR too,
# so they are scored apart).
import glob, re, sys, wave
import numpy as np
# The machine's torchvision does not match its torch; Whisper needs neither.
import importlib.util
_find = importlib.util.find_spec
importlib.util.find_spec = lambda name, *a, **k: None if name.startswith("torchvision") else _find(name, *a, **k)
import torch
from transformers import WhisperForConditionalGeneration, WhisperProcessor

proc = WhisperProcessor.from_pretrained("openai/whisper-base.en")
model = WhisperForConditionalGeneration.from_pretrained("openai/whisper-base.en").eval()

def asr(x, **_):
    feats = proc(x, sampling_rate=16000, return_tensors="pt").input_features
    with torch.no_grad():
        ids = model.generate(feats, max_new_tokens=440)
    return {"text": proc.batch_decode(ids, skip_special_tokens=True)[0]}
sentences = open("sentences.txt").read().splitlines()

def load(paths):
    xs = []
    for p in paths:
        w = wave.open(p)
        x = np.frombuffer(w.readframes(w.getnframes()), dtype=np.int16).astype(np.float32) / 32767
        if w.getframerate() != 16000:
            t = np.arange(0, len(x), w.getframerate() / 16000)
            x = np.interp(t, np.arange(len(x)), x)
        xs.append(x)
    return np.concatenate(xs)

def words(s):
    return re.sub(r"[^a-z0-9 ]", " ", s.lower()).split()

def wer(ref, hyp):
    r, h = words(ref), words(hyp)
    d = np.zeros((len(r) + 1, len(h) + 1), dtype=int)
    d[:, 0] = range(len(r) + 1); d[0, :] = range(len(h) + 1)
    for i in range(1, len(r) + 1):
        for j in range(1, len(h) + 1):
            d[i, j] = min(d[i-1, j] + 1, d[i, j-1] + 1, d[i-1, j-1] + (r[i-1] != h[j-1]))
    return d[-1, -1], len(r)

voices = {
    "kokoro fp32": lambda i: sorted(glob.glob(f"out/model-af_heart-{i:02}-*.wav")),
    "kokoro fp16": lambda i: sorted(glob.glob(f"out/model_fp16-af_heart-{i:02}-*.wav")),
    "kokoro 8-bit": lambda i: sorted(glob.glob(f"out/model_quantized-af_heart-{i:02}-*.wav")),
    "espeak-ng (system)": lambda i: [f"out-espeak/{i:02}.wav"],
}
maths = lambda s: any(k in s for k in ["sub ", "is equal to", "open paren", "fraction"])
for name, files in voices.items():
    tot = {"text": [0, 0], "maths": [0, 0]}
    for i, s in enumerate(sentences):
        hyp = asr(load(files(i)), generate_kwargs={"language": None} if False else {})["text"]
        e, n = wer(s, hyp)
        k = "maths" if maths(s) else "text"
        tot[k][0] += e; tot[k][1] += n
        if name == "kokoro fp32" and i in (2, 5, 23): print("   ", s, "->", hyp, file=sys.stderr)
    print(f"{name:20s} word errors: text {tot['text'][0]/tot['text'][1]:.1%}, maths {tot['maths'][0]/tot['maths'][1]:.1%}")
