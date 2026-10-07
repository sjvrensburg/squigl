# Misaki's neural fallback (PeterReid/graphemes_to_phonemes_en_us, BART) as two ONNX
# graphs -- encoder, and a decoder taking the encoder's states and the tokens so far
# -- then greedy decoding through ONNX Runtime checked against the PyTorch model.
import importlib.util, os, time
_find = importlib.util.find_spec
importlib.util.find_spec = lambda n, *a, **k: None if n.startswith("torchvision") else _find(n, *a, **k)
import numpy as np, torch, onnxruntime as ort
from transformers import BartForConditionalGeneration

name = "PeterReid/graphemes_to_phonemes_en_us"
m = BartForConditionalGeneration.from_pretrained(name).eval()
cfg = m.config
print("params", sum(p.numel() for p in m.parameters()), "layers", cfg.encoder_layers, cfg.decoder_layers, "d_model", cfg.d_model)
g2t = {g: i for i, g in enumerate(cfg.grapheme_chars)}
t2p = {i: p for i, p in enumerate(cfg.phoneme_chars)}

class Enc(torch.nn.Module):
    def __init__(s): super().__init__(); s.e = m.get_encoder()
    def forward(s, ids): return s.e(input_ids=ids).last_hidden_state
class Dec(torch.nn.Module):
    def __init__(s): super().__init__(); s.d = m.get_decoder(); s.h = m.lm_head; s.b = m.final_logits_bias
    def forward(s, ids, enc): return s.h(s.d(input_ids=ids, encoder_hidden_states=enc).last_hidden_state) + s.b

ids = torch.tensor([[1, 5, 6, 7, 2]])
enc = Enc()(ids)
os.makedirs("g2p", exist_ok=True)
torch.onnx.export(Enc(), (ids,), "g2p/encoder.onnx", input_names=["ids"], output_names=["enc"],
                  dynamic_axes={"ids": {1: "n"}, "enc": {1: "n"}}, opset_version=17)
torch.onnx.export(Dec(), (torch.tensor([[cfg.decoder_start_token_id]]), enc), "g2p/decoder.onnx",
                  input_names=["ids", "enc"], output_names=["logits"],
                  dynamic_axes={"ids": {1: "t"}, "enc": {1: "n"}, "logits": {1: "t"}}, opset_version=17)
print({f: os.path.getsize("g2p/" + f) for f in os.listdir("g2p")})

e = ort.InferenceSession("g2p/encoder.onnx"); d = ort.InferenceSession("g2p/decoder.onnx")
def onnx_g2p(word):
    x = np.array([[1] + [g2t.get(c, 3) for c in word] + [2]], dtype=np.int64)
    h = e.run(None, {"ids": x})[0]
    out = [cfg.decoder_start_token_id]
    for _ in range(64):
        logits = d.run(None, {"ids": np.array([out], dtype=np.int64), "enc": h})[0]
        nxt = int(logits[0, -1].argmax()); out.append(nxt)
        if nxt == cfg.eos_token_id: break
    return "".join(t2p.get(t, "") for t in out if t > 3)
def torch_g2p(word):
    x = torch.tensor([[1] + [g2t.get(c, 3) for c in word] + [2]])
    with torch.no_grad(): g = m.generate(input_ids=x)
    return "".join(t2p.get(t, "") for t in g[0].tolist() if t > 3)
words = ["Imfume", "Ardocummed", "homogamous", "childi", "Naire", "softmax", "sklearn", "hetero", "genous", "Bayer", "eigh", "MAE"]
same = 0; t0 = time.time()
for w in words:
    a, b = onnx_g2p(w), torch_g2p(w); same += a == b
    print(f"{w:12s} {a:16s} {b}")
print(f"agree {same}/{len(words)}; onnx {1000*(time.time()-t0)/len(words)/2:.0f} ms a word (both, halved)")
