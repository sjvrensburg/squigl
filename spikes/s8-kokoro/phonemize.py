# sentences.txt -> phonemes.txt (US) and phonemes-gb.txt (British), by Misaki with
# its neural fallback for words not in its dictionaries.
from misaki import en

for british, out in [(False, "phonemes.txt"), (True, "phonemes-gb.txt")]:
    g2p = en.G2P(trf=False, british=british)
    lines = [g2p(line)[0] for line in open("sentences.txt").read().splitlines()]
    open(out, "w").write("\n".join(lines) + "\n")
