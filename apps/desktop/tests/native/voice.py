#!/usr/bin/env python3
"""Real Apple speech verification on macOS 26+. No speech or microphone mocks.

Run after npm run build. --microphone also opens the microphone briefly to check
capture readiness and finalization; microphone transcripts are never printed.
"""
import argparse
import json
from pathlib import Path
import queue
import subprocess
import tempfile
import threading
import time

parser = argparse.ArgumentParser()
parser.add_argument('--microphone', action='store_true')
parser.add_argument('--helper', type=Path, default=Path(__file__).resolve().parents[2] / 'resources/haarness-voice')
args = parser.parse_args()
helper = str(args.helper.resolve())
probe = json.loads(subprocess.check_output([helper, '--probe', '--locale', 'en-US'], text=True, timeout=15))
assert probe['supported'] and probe['onDevice']
assert probe['engine'] in ('SpeechTranscriber', 'DictationTranscriber'), probe
print('Native engine:', probe['engine'], flush=True)


def run(arguments, control=None):
    child = subprocess.Popen([helper, '--locale', 'en-US', *arguments], stdin=subprocess.PIPE,
                             stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True)
    events = []
    output = queue.Queue()

    def read():
        for line in child.stdout:
            output.put(json.loads(line))
        output.put(None)

    threading.Thread(target=read, daemon=True).start()
    deadline = time.monotonic() + 180
    if control == 'cancel':
        child.stdin.write('cancel\n')
        child.stdin.flush()
        deadline = time.monotonic() + 5
    try:
        while True:
            event = output.get(timeout=max(.1, deadline - time.monotonic()))
            if event is None:
                break
            events.append(event)
            assert event['type'] != 'error', event
            if event['type'] == 'preparing':
                print(event['message'], flush=True)
            if event['type'] == 'started' and control == 'stop':
                # Exercise the tap and its format converter before finalization.
                time.sleep(.5)
                child.stdin.write('stop\n')
                child.stdin.flush()
                deadline = time.monotonic() + 10
        assert child.wait(timeout=5) == 0
        assert events[-1]['type'] == 'ended', events
        return events
    finally:
        if child.poll() is None:
            child.kill()
            child.wait()


with tempfile.TemporaryDirectory(prefix='haarness-native-voice-') as directory:
    original = Path(directory) / 'speech.aiff'
    converted = Path(directory) / 'speech.wav'
    subprocess.run(['say', '-v', 'Samantha', '-o', str(original),
                    'The quick brown fox jumps over the lazy dog. Please review this code.'], check=True)
    subprocess.run(['afconvert', '-f', 'WAVE', '-d', 'LEI16@16000', str(original), str(converted)], check=True)
    for audio in (original, converted):
        events = run(['--file', str(audio)])
        transcripts = [event for event in events if event['type'] == 'transcript']
        assert transcripts[-1]['isFinal']
        text = transcripts[-1]['text'].lower()
        assert all(word in text for word in ('quick', 'brown', 'fox', 'lazy', 'review', 'code')), text
        assert text.count('quick') == 1 and text.count('review') == 1, text
        print(f'PASS {audio.suffix}: real transcription, cumulative text, multiple sentences, and clean exit.', flush=True)
    run(['--file', str(original)], control='cancel')
    print('PASS: cancellation during preparation.', flush=True)
if args.microphone:
    events = run([], control='stop')
    assert any(event['type'] == 'started' for event in events)
    print('PASS: microphone capture, conversion, stop, and finalization.', flush=True)
