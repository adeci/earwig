# Test audio

Synthetic speech for engine tests, `earwig transcribe`, and `EARWIG_AUDIO_FILE`
in the VM tests. 16 kHz, mono, 16-bit PCM WAV, with 0.25 s of silence at each
end.

| File | Duration | Text |
| --- | --- | --- |
| `quick-brown-fox.wav` | 3.4 s | The quick brown fox jumps over the lazy dog. |
| `dictation-sentences.wav` | 8.8 s | Dictation should feel instant. Hold the key, say what you mean, and let go. The words appear where you are typing, all at once. |

## Provenance and license

Generated with [Piper](https://github.com/OHF-Voice/piper1-gpl) 1.8.0 (nixpkgs
`piper-tts`) using the `en_US-ljspeech-medium` voice from
[rhasspy/piper-voices](https://huggingface.co/rhasspy/piper-voices) at commit
`e7229f7d33a10e145e109b886d661984201ac3c4` (MIT). The voice was trained on the
[LJ Speech dataset](https://keithito.com/LJ-Speech-Dataset/), which is in the
public domain. The generated audio may be redistributed under this
repository's license.

```bash
rev=e7229f7d33a10e145e109b886d661984201ac3c4
base=https://huggingface.co/rhasspy/piper-voices/resolve/$rev/en/en_US/ljspeech/medium
curl -LO $base/en_US-ljspeech-medium.onnx -LO $base/en_US-ljspeech-medium.onnx.json
echo "The quick brown fox jumps over the lazy dog." > fox.txt
nix shell nixpkgs#piper-tts -c piper -m en_US-ljspeech-medium.onnx -i fox.txt -f fox.wav
nix shell nixpkgs#sox -c sox fox.wav -r 16000 -c 1 -b 16 quick-brown-fox.wav pad 0.25 0.25
# dictation-sentences.wav: same, with `--sentence-silence 0.3` for piper.
```
