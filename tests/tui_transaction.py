"""Native libalpm integration: only copies of the repository's fixture DB are modified."""
import json, os, shutil, socket, subprocess, sys, tempfile
import functools, http.server, threading
from pathlib import Path
BINARY=Path(sys.argv[1]).resolve()
ROOT=Path(__file__).resolve().parent.parent
with tempfile.TemporaryDirectory(prefix='paru-native-transaction-') as temporary:
    root=Path(temporary)
    db=root/'db';shutil.copytree(ROOT/'testdata/db',db)
    # Upstream resolver fixtures omit file lists; libalpm reinstall preparation needs them.
    for package in (db/'local').iterdir():
        if package.is_dir() and not (package/'files').exists(): (package/'files').write_text('%FILES%\n\n')
    (root/'hooks').mkdir();(root/'cache').mkdir()
    configuration=f'''[options]
RootDir = {root}
DBPath = {db}
LogFile = {root}/pacman.log
CacheDir = {root}/cache
HookDir = {root}/hooks
Architecture = x86_64
SigLevel = Never
[core]
Server = file://{ROOT}/testdata/db/sync
[extra]
Server = file://{ROOT}/testdata/db/sync
[community]
Server = file://{ROOT}/testdata/db/sync
'''
    request=root/'request.json'
    request.write_text(json.dumps(dict(configuration=configuration,operation='remove',options=[['dbonly',None],['noscriptlet',None]],targets=['polybar'],assume_installed=[])))
    listener=socket.socket(socket.AF_UNIX,socket.SOCK_STREAM)
    endpoint=str(root/'frontend.sock');listener.bind(endpoint);listener.listen(1);listener.settimeout(15)
    # Model a dependency graph in the disposable DB; verify removal scope at prepare.
    graph = {
        'removal-auto': (1, []),
        'removal-explicit': (0, []),
        'removal-target': (0, ['removal-auto', 'removal-explicit']),
        'removal-client': (0, ['removal-target']),
    }
    for name, (reason, dependencies) in graph.items():
        package=db/'local'/f'{name}-1-1';package.mkdir()
        fields={'NAME':name,'VERSION':'1-1','ARCH':'x86_64','REASON':str(reason),'SIZE':'1024','DEPENDS':'\n'.join(dependencies)}
        (package/'desc').write_text(''.join(f'%{key}%\n{value}\n\n' for key,value in fields.items()))
        (package/'files').write_text('%FILES%\n\n')
    for options, targets, expected in [
        ([], ['removal-target','removal-client'], {'removal-target','removal-client'}),
        (['s'], ['removal-target','removal-client'], {'removal-target','removal-client','removal-auto'}),
        (['s','c'], ['removal-target'], {'removal-target','removal-client','removal-auto'}),
        (['s','s','c','n'], ['removal-target'], set(graph)),
        (['unneeded'], ['removal-target'], set()),
    ]:
        request.write_text(json.dumps(dict(configuration=configuration,operation='remove',options=[[key,None] for key in ['dbonly','noscriptlet']+options],targets=targets,assume_installed=[])))
        worker=subprocess.Popen([str(BINARY),'--alpm-worker',endpoint,str(request)],stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)
        connection,_=listener.accept();connection.settimeout(15);reader=connection.makefile('r');plans=[]
        try:
            for line in reader:
                message=json.loads(line)
                if message.get('notice'):continue
                plans.append(message)
                assert {p['name'] for p in message['plan']}==expected,(options,message)
                assert all(p['source']=='REMOVE' for p in message['plan'])
                connection.sendall(b'{"value":"no","cancel":false}\n')
            out,err=worker.communicate(timeout=15)
            assert bool(plans)==bool(expected),(options,out,err)
            assert (worker.returncode==0)==(not expected),(options,out,err)
            assert not (db/'db.lck').exists()
            assert all((db/'local'/f'{name}-1-1').exists() for name in graph)
        finally:
            reader.close();connection.close()
            if worker.poll() is None:worker.kill();worker.wait()
    request.write_text(json.dumps(dict(configuration=configuration,operation='remove',options=[['dbonly',None],['noscriptlet',None]],targets=['polybar'],assume_installed=[])))
    original={p.name:p.read_bytes() for p in (db/'local/polybar-1.0.0-1').iterdir() if p.is_file()}
    for answer in [dict(value='no',cancel=False),dict(value='',cancel=True),None,dict(value='yes',cancel=False)]:
        worker=subprocess.Popen([str(BINARY),'--alpm-worker',endpoint,str(request)],stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)
        connection,_=listener.accept();connection.settimeout(15)
        reader=connection.makefile('r')
        plan_seen=False
        try:
            for line in reader:
                message=json.loads(line)
                if message.get('notice'): continue
                assert not plan_seen, message
                plan_seen=True
                assert message['plan']==[dict(name='polybar',version='1.0.0-1',source='REMOVE')],message
                assert 'Freed disk space' in message['text'] and 'Remove the prepared packages?' in message['text']
                assert message['default'] is True
                assert (db/'db.lck').exists(),'Plan must remain locked while awaiting confirmation'
                if answer is None:
                    connection.shutdown(socket.SHUT_RDWR);break
                connection.sendall((json.dumps(answer)+'\n').encode())
            out,err=worker.communicate(timeout=15)
            assert plan_seen,(out,err)
            assert not (db/'db.lck').exists(),err
            if answer and answer['value']=='yes':
                assert worker.returncode==0,(out,err)
                assert not (db/'local/polybar-1.0.0-1').exists()
            else:
                assert worker.returncode!=0,(out,err)
                assert {p.name:p.read_bytes() for p in (db/'local/polybar-1.0.0-1').iterdir() if p.is_file()}==original
        finally:
            reader.close();connection.close()
            if worker.poll() is None:worker.kill();worker.wait()
    for operation,targets,answer,expected in [
        ('sync',['core/pacman'],'no','pacman'),
        ('upgrade',[str(ROOT/'testdata/repo/polybar-1.0.0-1-x86_64.pkg.tar.zst')],'yes','polybar'),
    ]:
        request.write_text(json.dumps(dict(configuration=configuration,operation=operation,options=[['dbonly',None],['noscriptlet',None]],targets=targets,assume_installed=[])))
        worker=subprocess.Popen([str(BINARY),'--alpm-worker',endpoint,str(request)],stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)
        connection,_=listener.accept();connection.settimeout(15);reader=connection.makefile('r');plan_seen=False;messages=[]
        try:
            for line in reader:
                message=json.loads(line);messages.append(message)
                if message.get('notice'):continue
                assert any(p['name']==expected for p in message['plan']),message
                assert not plan_seen;plan_seen=True
                assert (db/'db.lck').exists()
                connection.sendall((json.dumps(dict(value=answer,cancel=False))+'\n').encode())
            out,err=worker.communicate(timeout=15)
            assert plan_seen,(operation,out,err,messages)
            assert (worker.returncode==0)==(answer=='yes'),(out,err)
            assert not (db/'db.lck').exists()
            if operation=='upgrade':assert (db/'local/polybar-1.0.0-1').exists()
        finally:
            reader.close();connection.close()
            if worker.poll() is None:worker.kill();worker.wait()
    # Exercise real libalpm HTTP download callbacks into an isolated cache.
    shutil.copyfile(ROOT/'testdata/repo/repo.db.tar.gz', db/'sync/fixture.db')
    class QuietHTTP(http.server.SimpleHTTPRequestHandler):
        def log_message(self, *args): pass
    server=http.server.ThreadingHTTPServer(('127.0.0.1',0),functools.partial(QuietHTTP,directory=str(ROOT/'testdata/repo')))
    threading.Thread(target=server.serve_forever,daemon=True).start()
    download_config=configuration+f'\n[fixture]\nServer = http://127.0.0.1:{server.server_port}\n'
    request.write_text(json.dumps(dict(configuration=download_config,operation='sync',options=[['downloadonly',None],['nodeps',None]],targets=['fixture/polybar'],assume_installed=[])))
    worker=subprocess.Popen([str(BINARY),'--alpm-worker',endpoint,str(request)],stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)
    connection,_=listener.accept();connection.settimeout(15);reader=connection.makefile('r');downloads=[]
    try:
        for line in reader:
            message=json.loads(line)
            if message.get('download'):downloads.append(message['download'])
            if message.get('notice'):continue
            assert message['default'] is True
            connection.sendall(b'{"value":"yes","cancel":false}\n')
        out,err=worker.communicate(timeout=15)
        assert worker.returncode==0,(out,err,downloads)
        assert any(e['kind']=='start' and e['total']>0 for e in downloads),downloads
        assert any(e['kind']=='progress' and e['downloaded']>0 for e in downloads),downloads
        assert any(e['kind']=='completed' and not e['failed'] for e in downloads),downloads
        filename='polybar-1.0.0-1-x86_64.pkg.tar.zst'
        assert (root/'cache'/filename).read_bytes()==(ROOT/'testdata/repo'/filename).read_bytes()
        assert not (db/'db.lck').exists()
    finally:
        reader.close();connection.close();server.shutdown();server.server_close()
        if worker.poll() is None:worker.kill();worker.wait()
    # Askpass uses a dedicated structured connection; the fixture password is
    # returned only to the calling process, never sent through the tool terminal.
    askpass=root/'paru-tui-askpass';askpass.symlink_to(BINARY)
    child=subprocess.Popen([str(askpass),'sudo password:'],env=dict(os.environ,PARU_TUI_SOCKET=endpoint),stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)
    connection,_=listener.accept();connection.settimeout(15)
    reader=connection.makefile('r');message=json.loads(reader.readline())
    assert message['secret'] and message['default'] is None
    connection.sendall(b'{"value":"fixture-secret","cancel":false}\n')
    out,err=child.communicate(timeout=10)
    assert child.returncode==0 and out=='fixture-secret\n' and not err
    reader.close();connection.close();listener.close()
print('PASS: native prepared plan, lock lifetime, decline/cancel/disconnect, repository plan, local archive install, isolated DB-only commit, HTTP download progress, and askpass protocol')
