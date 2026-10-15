#!/usr/bin/env python3
"""Opt-in provider discovery, tool continuation and multi-turn verification.

Use environment API keys and/or --auth-file pointing at an existing Haarness
credential store. Only synthetic fixtures are sent. Credentials are never copied
into evidence; a supplied store may refresh OAuth credentials normally.

--scenarios picks what each model must do (default: tools):
  tools      read two files in sequence, then read another in a second turn
  reasoning  answer a small arithmetic question with visible reasoning at the highest effort
  image      name the colour of an attached PNG
  schema     return a file's marker as workflow structured output (ctx.agent schema)
  cost       report usage and a price for every turn
  fallback   also run the tool turns with a level the model lacks (none/minimal); the server must pick a valid one
"""
import argparse
import base64
import concurrent.futures
import json
import os
import pathlib
import queue
import re
import secrets
import subprocess
import tempfile
import threading
import time
import urllib.request
import uuid
import zlib

CANDIDATES = {
    'openai': ['gpt-6.1-sol', 'gpt-6-astra', 'gpt-6-sol', 'gpt-6-luna', 'gpt-5.6-sol', 'gpt-5.6-terra', 'gpt-5.6-luna', 'gpt-5.5', 'gpt-5.4-mini', 'gpt-4.1'],
    'anthropic': ['claude-opus-5-5', 'claude-sonnet-5-5', 'claude-haiku-5-5', 'claude-fable-5-1', 'claude-opus-5', 'claude-sonnet-5', 'claude-fable-5', 'claude-opus-4-8', 'claude-opus-4-7', 'claude-opus-4-6', 'claude-sonnet-4-6', 'claude-haiku-4-5', 'claude-sonnet-4-5-20250929', 'claude-opus-4-5-20251101'],
    'gemini': ['gemini-4-argon', 'gemini-3.8-flash', 'gemini-3.7-flash', 'gemini-3.6-flash', 'gemini-3.5-flash', 'gemini-3.5-flash-lite', 'gemini-3.1-flash-lite', 'gemini-3.1-pro-preview', 'gemini-3-flash-preview', 'gemini-2.5-pro', 'gemini-2.5-flash', 'gemini-2.5-flash-lite'],
    'xai': ['grok-4.7', 'grok-4.3'],
    'deepseek': ['deepseek-flash', 'deepseek-v4-pro'],
    'mistral': ['mistral-large-4-0', 'mistral-medium-3-5', 'mistral-small-latest', 'mistral-large-latest'],
    'groq': ['openai/gpt-oss-120b', 'openai/gpt-oss-20b'],
}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--auth-file', type=pathlib.Path)
    parser.add_argument('--providers', default=','.join(CANDIDATES))
    parser.add_argument('--models', help='Comma-separated provider/model pairs; absent models fail')
    parser.add_argument('--workers', type=int, default=2)
    parser.add_argument('--timeout', type=int, default=180)
    parser.add_argument('--scenarios', default='tools', help='Comma-separated: tools,reasoning,image,schema,cost,fallback')
    args = parser.parse_args()
    scenarios = set(args.scenarios.split(','))
    binary = pathlib.Path(os.environ.get('HAARNESS_BIN', 'target/debug/haarness')).resolve()
    root = pathlib.Path(tempfile.mkdtemp(prefix='haarness-models-live-')).resolve()
    workspace = root/'workspace'
    (workspace/'.haarness').mkdir(parents=True)
    (workspace/'.haarness/config.toml').write_text('[permissions]\nread="allow"\nexternal_directory="deny"\n[providers.ollama]\nenabled=false\n[providers.lmstudio]\nenabled=false\n')
    token = secrets.token_hex(24)
    env = {**os.environ, 'HAARNESS_DATA_DIR': str(root/'profile'), 'HAARNESS_SERVER_TOKEN': token}
    env.pop('HAARNESS_TEST_PROVIDER', None)
    auth_link = root/'profile/data/auth.json'
    if args.auth_file:
        source = args.auth_file.expanduser().resolve(strict=True)
        auth_link.parent.mkdir(parents=True)
        auth_link.symlink_to(source)
    ready = queue.Queue()
    log = (root/'server.log').open('w')
    process = subprocess.Popen([str(binary), '--workspace', str(workspace), '--trust-project', 'serve', '--port', '0'], env=env, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=log, text=True)

    def reader():
        for line in process.stdout:
            found = re.search(r'Haarness server listening on (http://\S+)', line)
            if found:
                ready.put(found[1])
        ready.put(None)

    threading.Thread(target=reader, daemon=True).start()
    results, skipped = [], []
    print('Evidence: '+str(root), flush=True)
    try:
        url = ready.get(timeout=90)
        assert url, 'Private server failed to start'

        def api(path, value=None, method=None):
            request = urllib.request.Request(url+path, data=None if value is None else json.dumps(value).encode(), headers={'Authorization': 'Bearer '+token, 'Content-Type': 'application/json'}, method=method)
            with urllib.request.urlopen(request, timeout=30) as response:
                body = response.read()
                return json.loads(body) if body else None

        catalog = api('/v1/providers/refresh', {}, 'POST')
        selected = {p['id']: p for p in catalog if p['id'] in args.providers.split(',') and p['authenticated']}
        (root/'catalog.json').write_text(json.dumps(selected, indent=2))
        for provider in selected.values():
            assert len({m['id'] for m in provider['models']}) == len(provider['models'])
            print('CATALOG '+provider['id']+': '+str(len(provider['models']))+' unique models', flush=True)
        cases = []
        explicit = [value.split('/', 1) for value in args.models.split(',')] if args.models else None
        requested = explicit or [(provider, model) for provider in args.providers.split(',') for model in CANDIDATES.get(provider, [])]
        for provider, model in requested:
            info = next((m for m in selected.get(provider, {}).get('models', []) if m['id'] == model), None)
            if info is None:
                item = {'provider': provider, 'model': model, 'status': 'unavailable', 'reason': 'Not in authenticated live catalog'}
                (results if explicit else skipped).append(item)
                continue
            efforts = info['reasoningEfforts']
            effort = 'low' if 'low' in efforts else ('high' if 'high' in efforts else None)
            cases.append((provider, model, effort))
            if 'reasoning' in scenarios and efforts:
                cases.append((provider, model, 'reasoning:'+efforts[-1]))
            missing = next((level for level in ['none', 'minimal'] if efforts and level not in efforts), None)
            if 'fallback' in scenarios and missing:
                cases.append((provider, model, missing))
            # Verify the newly supported reasoning-off contract separately.
            if provider == 'openai' and model in ['gpt-6-sol', 'gpt-6-luna'] and 'none' in efforts:
                cases.append((provider, model, 'none'))
        assert cases, 'No requested provider/model is available'

        def png(width, height, rgb):
            def chunk(kind, data):
                return len(data).to_bytes(4, 'big')+kind+data+zlib.crc32(kind+data).to_bytes(4, 'big')
            rows = b''.join(b'\x00'+bytes(rgb)*width for _ in range(height))
            header = width.to_bytes(4, 'big')+height.to_bytes(4, 'big')+bytes([8, 2, 0, 0, 0])
            return b'\x89PNG\r\n\x1a\n'+chunk(b'IHDR', header)+chunk(b'IDAT', zlib.compress(rows))+chunk(b'IEND', b'')

        def wait_turn(admitted):
            deadline = time.monotonic()+args.timeout
            while time.monotonic() < deadline:
                status = api('/v1/turns/'+admitted['id'])
                if status['status'] in ['completed', 'failed', 'cancelled', 'interrupted']:
                    return status
                time.sleep(.15)
            return status

        def run_reasoning(thread, effort, report):
            admitted = api('/v1/threads/'+thread['id']+'/turns', {'requestId': str(uuid.uuid4()), 'reasoningEffort': effort, 'text': 'Without tools: what is the smallest positive integer n with n mod 7 = 3, n mod 11 = 5 and n mod 13 = 4? Check each remainder, then reply with only n.'})
            status = wait_turn(admitted)
            assert status['status'] == 'completed', 'Turn '+status['status']
            items = api('/v1/threads/'+thread['id']+'/items')
            texts = [item['data'].get('text', '') for item in items if item['data']['type'] == 'assistant_message']
            reasoning = ''.join(item['data'].get('text', '') for item in items if item['data']['type'] == 'reasoning')
            assert texts and '654' in texts[-1], 'Wrong or missing answer: '+(texts[-1][:200] if texts else '')
            # OpenAI's `auto` summaries may be empty when the model reasons only briefly.
            assert reasoning.strip() or thread['provider'] == 'openai', 'No visible reasoning at effort '+effort
            report.update(reasoning_chars=len(reasoning))

        def run_image(thread, effort, report):
            data = 'data:image/png;base64,'+base64.b64encode(png(64, 64, (220, 30, 30))).decode()
            value = {'requestId': str(uuid.uuid4()), 'text': 'What single colour fills the attached image? Reply with one lowercase word.', 'attachments': [data]}
            if effort:
                value['reasoningEffort'] = effort
            status = wait_turn(api('/v1/threads/'+thread['id']+'/turns', value))
            assert status['status'] == 'completed', 'Turn '+status['status']
            items = api('/v1/threads/'+thread['id']+'/items')
            texts = [item['data'].get('text', '') for item in items if item['data']['type'] == 'assistant_message']
            assert texts and 'red' in texts[-1].lower(), 'Image colour not recognized: '+(texts[-1][:200] if texts else '')

        def run_schema(provider, model, effort, directory, report):
            marker = 'schema-'+secrets.token_hex(8)
            target = directory/'schema.txt'
            target.write_text('The verification marker is '+marker+'.\n')
            source = (
                "export const meta = { name: 'schema-check', phases: ['read'] };\n"
                "phase('read');\n"
                "const result = await agent('Use the read tool to read "+str(target.relative_to(workspace))+" and return the verification marker it contains.', "
                "{ schema: { type: 'object', properties: { marker: { type: 'string' } }, required: ['marker'], additionalProperties: false } });\n"
                "return result.data;\n"
            )
            value = {'source': source, 'model': provider+'/'+model}
            if effort:
                value['variant'] = effort
            run = api('/v1/workflows/run', value)
            deadline = time.monotonic()+args.timeout
            while run['status'] == 'running' and time.monotonic() < deadline:
                time.sleep(.3)
                run = api('/v1/workflows/runs/'+run['id'])
            report['workflow'] = run['id']
            assert run['status'] == 'completed', 'Workflow '+run['status']+': '+json.dumps(run.get('error') or run.get('logs', [])[-3:])[:400]
            assert run.get('result') == {'marker': marker}, 'Structured result: '+json.dumps(run.get('result'))[:200]

        def check_cost(thread, report):
            usage = api('/v1/threads/'+thread['id']+'/usage')
            report.update(input_tokens=usage['inputTokens'], output_tokens=usage['outputTokens'], cache_read_tokens=usage['cacheReadTokens'], cache_write_tokens=usage['cacheWriteTokens'], cost_usd=usage['costUsd'])
            assert usage['inputTokens'] > 0 and usage['outputTokens'] > 0, 'No usage reported'
            assert usage['costUsd'] is not None and not usage['costIncomplete'], 'Turn without a price'

        def run_case(case):
            provider, model, effort = case
            label = provider+'/'+model+'@'+str(effort)
            if effort and effort.startswith('reasoning:'):
                effort = effort.split(':', 1)[1]
                report = {'provider': provider, 'model': model, 'effort': effort, 'scenario': 'reasoning'}
                started = time.monotonic()
                thread = None
                try:
                    thread = api('/v1/threads', {'title': 'Reasoning verification '+label, 'provider': provider, 'model': model})
                    run_reasoning(thread, effort, report)
                    if 'cost' in scenarios:
                        check_cost(thread, report)
                    report.update(status='passed')
                except Exception as error:
                    report.update(status='failed', error=str(error)[:500])
                finally:
                    if thread:
                        items = api('/v1/threads/'+thread['id']+'/items')
                        (root/(thread['id']+'.json')).write_text(json.dumps(items, indent=2))
                        report['errors'] = [item['data'] for item in items if item['data']['type'] == 'error']
                    report['seconds'] = round(time.monotonic()-started, 2)
                print(report['status'].upper()+' '+label+' '+str(report['seconds'])+'s '+report.get('error', ''), flush=True)
                return report
            directory = workspace/uuid.uuid4().hex
            directory.mkdir()
            marker = 'proof-'+secrets.token_hex(10)
            second = directory/('detail-'+secrets.token_hex(5)+'.txt')
            first = directory/'start.txt'
            first.write_text('The next file is '+str(second.relative_to(workspace))+'. Read it to learn the verification marker.\n')
            second.write_text(marker+'\n')
            report = {'provider': provider, 'model': model, 'effort': effort}
            started = time.monotonic()
            thread = None
            try:
                thread = api('/v1/threads', {'title': 'Model verification '+label, 'provider': provider, 'model': model})
                report['thread'] = thread['id']

                def turn(text, proof):
                    value = {'requestId': str(uuid.uuid4()), 'text': text}
                    if effort is not None:
                        value['reasoningEffort'] = effort
                    admitted = api('/v1/threads/'+thread['id']+'/turns', value)
                    deadline = time.monotonic()+args.timeout
                    while time.monotonic() < deadline:
                        status = api('/v1/turns/'+admitted['id'])
                        if status['status'] in ['completed', 'failed', 'cancelled', 'interrupted']:
                            break
                        time.sleep(.15)
                    assert status['status'] == 'completed', 'Turn '+status['status']
                    items = api('/v1/threads/'+thread['id']+'/items')
                    texts = [item['data'].get('text', '') for item in items if item['data']['type'] == 'assistant_message']
                    assert texts and proof in texts[-1], 'Missing random file proof in final answer'
                    return items

                done = []
                if 'tools' in scenarios:
                    run_tools(thread, directory, turn, marker, first, report)
                    done.append('tools')
                if 'image' in scenarios:
                    run_image(thread, effort, report)
                    done.append('image')
                if 'schema' in scenarios:
                    run_schema(provider, model, effort, directory, report)
                    done.append('schema')
                if 'cost' in scenarios:
                    check_cost(thread, report)
                    done.append('cost')
                saved = api('/v1/threads/'+thread['id'])
                assert (saved['provider'], saved['model']) == (provider, model), 'Model selection changed'
                report.update(status='passed', scenarios=done)
            except Exception as error:
                report.update(status='failed', error=str(error)[:500])
            finally:
                if thread:
                    items = api('/v1/threads/'+thread['id']+'/items')
                    # Evidence contains synthetic conversation only, no auth values.
                    (root/(thread['id']+'.json')).write_text(json.dumps(items, indent=2))
                    report['errors'] = [item['data'] for item in items if item['data']['type'] == 'error']
                report['seconds'] = round(time.monotonic()-started, 2)
            print(report['status'].upper()+' '+label+' '+str(report['seconds'])+'s '+report.get('error', ''), flush=True)
            return report

        def run_tools(thread, directory, turn, marker, first, report):
            items = turn('Use the read tool to read '+str(first.relative_to(workspace))+', then read the file named inside it. Reply with only the verification marker from the second file.', marker)
            calls = [item for item in items if item['data']['type'] == 'tool_call']
            tool_results = [item for item in items if item['data']['type'] == 'tool_result']
            assert len(calls) >= 2 and len(tool_results) >= 2, 'Sequential tool round trip missing'
            next_marker = 'followup-'+secrets.token_hex(10)
            followup = directory/'followup.txt'
            followup.write_text(next_marker+'\n')
            items = turn('Now read '+str(followup.relative_to(workspace))+' with the read tool and reply with only its new marker.', next_marker)
            all_calls = [item for item in items if item['data']['type'] == 'tool_call']
            assert len(all_calls) > len(calls), 'Follow-up did not execute a new tool'
            report.update(tool_calls=len(all_calls), turns=2)

        with concurrent.futures.ThreadPoolExecutor(max_workers=max(1, args.workers)) as pool:
            for result in pool.map(run_case, cases):
                results.append(result)
                (root/'results.json').write_text(json.dumps({'results': results, 'skipped': skipped}, indent=2))
        refreshed = api('/v1/providers/refresh', {}, 'POST')
        assert set(p['id'] for p in refreshed) == set(p['id'] for p in catalog)
        passed = sum(result['status'] == 'passed' for result in results)
        print(json.dumps({'passed': passed, 'failed': len(results)-passed, 'unavailable': len(skipped), 'evidence': str(root)}), flush=True)
        return 0 if passed == len(results) else 1
    finally:
        process.terminate()
        try:
            process.wait(timeout=15)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait()
        log.close()
        if auth_link.is_symlink():
            auth_link.unlink()


if __name__ == '__main__':
    raise SystemExit(main())
