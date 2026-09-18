"""Read-only TUI integration checks with local mirrors/RPC and temporary settings."""
import codecs, fcntl, re, struct, termios, os, pty, select, signal, subprocess, sys, tempfile, time, unicodedata
import http.server, json, socket, threading, urllib.parse
from pathlib import Path

class Screen:
    """Track the cursor-addressed output emitted by Ratatui for assertions."""
    def __init__(self):
        self.cells = [[' '] * 160 for _ in range(60)]
        self.row = self.col = 0
        self.pending = ''
        self.decoder = codecs.getincrementaldecoder('utf-8')('replace')

    def feed(self, data):
        self.pending += self.decoder.decode(data)
        while self.pending:
            if self.pending.startswith('\x1b'):
                match = re.match(r'\x1b\[([0-?]*)([ -/]*)([@-~])', self.pending)
                if not match:
                    return
                params, _, cmd = match.groups()
                self.pending = self.pending[match.end():]
                if cmd in ('H', 'f'):
                    numbers = [int(n or '1') for n in params.split(';')]
                    self.row = min(59, max(0, numbers[0] - 1))
                    self.col = min(159, max(0, (numbers[1] if len(numbers) > 1 else 1) - 1))
                elif cmd == 'J' and params == '2':
                    self.cells = [[' '] * 160 for _ in range(60)]
                continue
            char, self.pending = self.pending[0], self.pending[1:]
            if char == '\r': self.col = 0
            elif char == '\n': self.row = min(59, self.row + 1)
            elif char >= ' ':
                self.cells[self.row][self.col] = char
                width = 2 if unicodedata.east_asian_width(char) in ('W', 'F') else 1
                if width == 2 and self.col < 159: self.cells[self.row][self.col+1] = ''
                self.col = min(159, self.col + width)

    def text(self):
        return '\n'.join(''.join(row).rstrip() for row in self.cells)

BINARY = Path(sys.argv[1]).resolve()
ROOT = Path(__file__).resolve().parent.parent

class RPC(http.server.BaseHTTPRequestHandler):
    version="999.0-1"
    shared_base=True
    def do_POST(self):
        self.params = self.rfile.read(int(self.headers.get('Content-Length','0'))).decode()
        self.do_GET()
    def do_GET(self):
        args=urllib.parse.parse_qs(getattr(self,'params',urllib.parse.urlsplit(self.path).query))
        names=args.get('arg[]', ['polybar'])
        if args.get('type') == ['search']: names=['polybar','polybar-tools'] if 'polybar' in args.get('arg', [''])[0] else []
        packages=[]
        for name in names:
            packages.append(dict(ID=1, Name=name, PackageBaseID=1, PackageBase='polybar' if self.shared_base or name.startswith('polybar') else name, Version=self.version,
                Description='Local integration fixture package', URL='https://example.org/polybar', NumVotes=1,
                Popularity=1.0, OutOfDate=1789516800, Maintainer=None, Submitter='test', FirstSubmitted=1600000000,
                LastModified=1789516800, Depends=['libc'], MakeDepends=['cmake'], OptDepends=['python: optional feature'], URLPath='/unused'))
        content=json.dumps(dict(type='multiinfo',results=packages)).encode()
        self.send_response(200);self.send_header('Content-Type','application/json');self.send_header('Content-Length',str(len(content)));self.end_headers();self.wfile.write(content)
    def log_message(self,*args): pass

with tempfile.TemporaryDirectory(prefix='paru-tui-smoke-') as temporary:
    root=Path(temporary)
    server=http.server.ThreadingHTTPServer(('127.0.0.1',0),RPC)
    threading.Thread(target=server.serve_forever,daemon=True).start()
    port=server.server_port
    pconf=root/'pacman.conf'
    pconf.write_text(f'''[options]
RootDir = {root}
DBPath = {ROOT}/testdata/db
LogFile = {root}/pacman.log
Architecture = x86_64
SigLevel = Never
[core]
Server = file://{ROOT}/testdata/db/sync
[extra]
Server = file://{ROOT}/testdata/db/sync
[community]
Server = file://{ROOT}/testdata/db/sync
''')
    conf=root/'paru.conf'
    conf.write_text(f'[options]\nPacmanConf = {pconf}\nAurRpcUrl = http://127.0.0.1:{port}/rpc/\n')
    env=dict(os.environ,PARU_CONF=str(conf),XDG_CONFIG_HOME=str(root/'config'),XDG_CACHE_HOME=str(root/'cache'),XDG_STATE_HOME=str(root/'state'),TERM='xterm-256color')
    for key in list(env):
        if key.lower() in ['http_proxy','https_proxy','all_proxy','no_proxy']: env.pop(key)
    opener=root/'xdg-open'
    opener.write_text('#!'+sys.executable+'\nimport pathlib,sys\npathlib.Path(__file__).with_name("opened-url").write_text(sys.argv[1])\n')
    opener.chmod(0o755)
    env['PATH']=str(root)+os.pathsep+env['PATH']
    pid,fd=pty.fork()
    if pid==0: os.execve(str(BINARY),[str(BINARY)],env)
    fcntl.ioctl(fd,termios.TIOCSWINSZ,struct.pack('HHHH',36,140,0,0))
    original=termios.tcgetattr(fd)
    screen=Screen();phase=0;deadline=time.monotonic()+60;raw=bytearray();screens=[]
    try:
        while time.monotonic()<deadline:
            ready=select.select([fd],[],[],.1)[0]
            if not ready and phase not in (514,515): continue
            try:data=os.read(fd,65536) if ready else b''
            except OSError:break
            if ready and not data:break
            raw.extend(data);screen.feed(data);view=screen.text();screens.append(view)
            if phase==0 and 'Scan complete' in view and 'polybar' in view:
                Path('/tmp/paru-tui-updates.txt').write_text('\n'.join(view.splitlines()[:36]))
                os.write(fd,b'4\r');phase=1
            elif phase==1 and 'Proxy URL' in view and 'SETTINGS' in view:
                os.write(fd,b'bad address\r');phase=20
            elif phase==20 and 'Proxy not saved' in view and 'bad address' in view:
                os.write(fd,b'\x15127.0.0.1:7890\r');phase=2
            elif phase==2 and 'Settings saved' in view and '127.0.0.1:7890' in view:
                os.write(fd,b'\x1b[B\r');phase=21
            elif phase==21 and 'Git network proxy: ON' in view:
                assert 'git_proxy = true' in (root/'config/paru-tui/settings.toml').read_text()
                os.write(fd,b'\r');phase=22
            elif phase==22 and 'Git network proxy: OFF' in view:
                assert 'git_proxy = false' in (root/'config/paru-tui/settings.toml').read_text()
                os.write(fd,b'\x1b[B\r');phase=23
            elif phase==23 and '界面语言' in view:
                assert 'zh-cn' in (root/'config/paru-tui/settings.toml').read_text()
                os.write(fd,b'1\t\r');phase=231
            elif phase==231 and '更新单个 AUR 包' in view and '$ paru -S --aur --' in view and '[ 是 ]' in view:
                assert '仅处理此 AUR 包' in view
                assert '$ paru -S --aur --' in view
                os.write(fd,b'\x1b');phase=232
            elif phase==232 and '更新单个 AUR 包' not in view:
                os.write(fd,b'\x1b[Z4\r');phase=24
            elif phase==24 and 'SETTINGS' in view:
                os.write(fd,b'\x1b[B\r');phase=241
            elif phase==241 and 'Theme color: Blue' in view:
                assert 'accent = "blue"' in (root/'config/paru-tui/settings.toml').read_text()
                os.write(fd,b'\r\r\r\r');phase=242
            elif phase==242 and 'Theme color: Teal' in view:
                assert 'accent = "teal"' in (root/'config/paru-tui/settings.toml').read_text()
                os.write(fd,b'1\tp');phase=3
            elif phase==3 and 'proxy ON' in view:
                os.write(fd,b'\r');phase=30
            elif phase==30 and 'UPDATE AUR PACKAGE' in view and '$ paru -S --aur --' in view and '[ YES ]' in view:
                Path('/tmp/paru-tui-confirm.txt').write_text('\n'.join(view.splitlines()[:36]))
                assert 'Only this AUR target' in view
                assert '$ paru -S --aur --' in view
                os.write(fd,b'\x1b');phase=31
            elif phase==31 and 'UPDATE AUR PACKAGE' not in view:
                os.write(fd,b'3/polybar\r');phase=4
            elif phase==4 and 'Architecture' in view and 'polybar' in view:
                assert 'AUR SEARCH' not in view
                package_row=next(line for line in view.splitlines() if '▸ polybar' in line)
                assert '1.0.0-1' in package_row.split('││')[0],package_row
                Path('/tmp/paru-tui-search.txt').write_text('\n'.join(view.splitlines()[:36]))
                os.write(fd,b't\x1b[C');phase=401
            elif phase==401 and 'TREE · MATCHED PACKAGES' in view and '├' in view:
                assert 'VERSION' in view.split('INSPECTOR')[0] or 'VERSION' in view
                Path('/tmp/paru-tui-tree.txt').write_text('\n'.join(view.splitlines()[:36]))
                os.write(fd,b'\x1b[Bd');phase=402
            elif phase==402 and 'REMOVAL OPTIONS' in view:
                os.write(fd,b'\r');phase=403
            elif phase==403 and 'REMOVE PACKAGE' in view and '$ paru -Rssc -- alsa-lib' in view:
                assert '$ paru -Rssc -- alsa-lib' in view,view
                os.write(fd,b'\x1b');phase=404
            elif phase==404 and 'REMOVE PACKAGE' not in view:
                os.write(fd,b'\x1b[A\x1b[Dt');phase=405
            elif phase==405 and 'INSTALLED' in view:
                os.write(fd,b'd');phase=40
            elif phase==40 and all(text in view for text in ['REMOVAL OPTIONS', 'Remove unused dependencies', 'Cascade to dependent packages']):
                assert 'REMOVE PACKAGE' not in view and '$ paru' not in view
                os.write(fd,b'\r');phase=41
            elif phase==41 and 'REMOVE PACKAGE' in view and '$ paru -Rssc -- polybar' in view:
                Path('/tmp/paru-tui-remove-confirm.txt').write_text('\n'.join(view.splitlines()[:36]))
                assert 'final removal plan' in view
                os.write(fd,b'\x1b');phase=42
            elif phase==42 and 'REMOVE PACKAGE' not in view:
                os.write(fd,b'\x1b[3~');phase=43
            elif phase==43 and 'REMOVAL OPTIONS' in view:
                os.write(fd,b'\x1b');phase=44
            elif phase==44 and 'REMOVAL OPTIONS' not in view:
                os.write(fd,b'\t');phase=5
            elif phase==5 and 'INSTALLED' in view:
                assert 'REPOSITORY CATALOG' not in view
                os.write(fd,b'2');phase=45
            elif phase==45 and 'Press / to search by name or description' in view:
                assert '▏' not in view
                os.write(fd,b'/bash\r');phase=50
            elif phase==50 and 'SEARCH RESULTS' in view and 'bash' in view and 'core' in view and 'VERSION & SOURCE' in view:
                Path('/tmp/paru-tui-install-official.txt').write_text('\n'.join(view.splitlines()[:36]))
                os.write(fd,b'\r');phase=501
            elif phase==501 and 'INSTALL PACKAGE' in view and '$ paru -S -- core/bash' in view:
                assert '-Syu' not in view
                os.write(fd,b'\x1b');phase=502
            elif phase==502 and 'INSTALL PACKAGE' not in view:
                os.write(fd,b'/\x15polybar\r');phase=51
            elif phase==51 and 'Unmaintained (orphaned)' in view and 'Flagged out-of-date' in view:
                assert 'aur' in view and '2026-09-16' in view and '[old][orphan]' in view,view
                Path('/tmp/paru-tui-install-aur.txt').write_text('\n'.join(view.splitlines()[:36]))
                os.write(fd,b'P');phase=514
            elif phase==514 and 'polybar' not in (root/'config/paru-tui/settings.toml').read_text():
                assert 'polybar' not in (root/'config/paru-tui/settings.toml').read_text()
                os.write(fd,b'p');phase=515
            elif phase==515 and 'polybar' in (root/'config/paru-tui/settings.toml').read_text():
                assert 'polybar' in (root/'config/paru-tui/settings.toml').read_text()
                os.write(fd,b'o\r');phase=511
            elif phase==511 and 'INSTALL PACKAGE' in view and '$ paru -S -- aur/polybar' in view:
                assert '-Syu' not in view
                os.write(fd,b'\x1b');phase=512
            elif phase==512 and 'INSTALL PACKAGE' not in view:
                # The opener runs asynchronously; release rendering can beat exec.
                opened_deadline=time.monotonic()+5
                while not (root/'opened-url').exists() and time.monotonic()<opened_deadline:
                    time.sleep(.01)
                assert (root/'opened-url').read_text()=='https://aur.archlinux.org/packages/polybar'
                os.write(fd,b'\t\t\r');phase=513
            elif phase==513 and 'INSTALL PACKAGE' not in view:
                os.write(fd,b'1a');phase=6
            elif phase==6 and 'UPDATE ALL' in view and '[ YES ]' in view:
                os.write(fd,b'\x1b');phase=7
            elif phase==7 and 'UPDATE ALL' not in view:
                os.write(fd,b'?');phase=70
            elif phase==70 and 'KEYBOARD SHORTCUTS' in view and 'Next / previous source or panel' in view:
                os.write(fd,b'\x1b');phase=71
            elif phase==71 and 'KEYBOARD SHORTCUTS' not in view:
                os.write(fd,b'5');phase=72
            elif phase==72 and 'SESSION ACTIVITY' in view and 'INFO' in view and 'ERROR' in view:
                assert 'INFO' in view and 'ERROR' in view,view
                os.write(fd,b'q');phase=8
        else:
            raise AssertionError(f'TUI timeout at phase {phase}\n'+screen.text())
        _,status=os.waitpid(pid,0)
        assert os.waitstatus_to_exitcode(status)==0,screen.text()
        assert phase==8,screen.text()
        assert (termios.tcgetattr(fd)[3]&(termios.ECHO|termios.ICANON))==(original[3]&(termios.ECHO|termios.ICANON))
        config=(root/'config/paru-tui/settings.toml').read_text()
        assert 'polybar' in config and 'http://127.0.0.1:7890' in config,config
        assert (root/'config/paru-tui/settings.toml').stat().st_mode & 0o777 == 0o600
    except BaseException:
        os.kill(pid,signal.SIGTERM)
        os.waitpid(pid,0)
        raise
    finally:
        os.close(fd);server.shutdown()

    # Invoke this fork's backend through the worker entry, using only a query.
    listener=socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    endpoint=str(root/'worker.sock');listener.bind(endpoint);listener.listen(1);listener.settimeout(10)
    worker=subprocess.Popen([str(BINARY),'--worker',endpoint,'--git','/usr/bin/git','-Q','polybar'],env=env,stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)
    connection,_=listener.accept()
    try:
        out,err=worker.communicate(timeout=15)
        assert worker.returncode==0 and 'polybar 1.0.0-1' in out,(out,err)
    finally:
        if worker.poll() is None: worker.kill();worker.wait()
        connection.close();listener.close()

    fake=root/'real-git' 
    fake.write_text('#!'+sys.executable+'\nimport os,sys,json\nprint(json.dumps({"args":sys.argv[1:],"proxy":os.getenv("https_proxy")}))\n')
    fake.chmod(0o755)
    helper=root/'git';helper.symlink_to(BINARY)
    policy=root/'policy.toml';policy.write_text('proxy_url="http://127.0.0.1:1080"\ngit_proxy=false\nproxy_bases=["polybar"]\n')
    helper_env=dict(env,PARU_TUI_POLICY=str(policy),PARU_TUI_REAL_GIT=str(fake),https_proxy='http://wrong.invalid')
    for base,expected in [('polybar','http://127.0.0.1:1080'),('other',None)]:
        result=subprocess.run([str(helper),'clone',f'https://example.org/{base}.git'],env=helper_env,text=True,capture_output=True,check=True)
        assert json.loads(result.stdout)['proxy']==expected,result.stdout
    policy.write_text('proxy_url="http://127.0.0.1:1080"\ngit_proxy=true\nproxy_bases=[]\n')
    result=subprocess.run([str(helper),'ls-remote','https://example.org/other.git'],env=helper_env,text=True,capture_output=True,check=True)
    assert json.loads(result.stdout)['proxy']=='http://127.0.0.1:1080'
    result=subprocess.run([str(helper),'ls-remote','git@example.org:other.git'],env=helper_env,text=True,capture_output=True)
    assert result.returncode!=0 and 'refusing' in result.stderr
    # End-to-end scan with unchanged AUR version and a changed tracked Git commit.
    # Exercise both global Git proxy and the per-base route, then a no-update scan.
    RPC.version='1.0.0-1'
    RPC.shared_base=False
    server=http.server.ThreadingHTTPServer(('127.0.0.1',0),RPC)
    threading.Thread(target=server.serve_forever,daemon=True).start()
    git=root/'scan-git'
    git.write_text('#!'+sys.executable+'\nimport json,os,pathlib,sys\n'
        'root=pathlib.Path(__file__).parent\n'
        'with (root/"git-requests").open("a") as f: f.write(json.dumps({"args":sys.argv[1:],"proxy":os.getenv("https_proxy")})+"\\n")\n'
        'print((root/"remote-commit").read_text().strip()+"\\tHEAD")\n')
    git.chmod(0o755)
    conf.write_text(f'[options]\nPacmanConf = {pconf}\nAurRpcUrl = http://127.0.0.1:{server.server_port}/rpc/\n[bin]\nGit = {git}\n')
    state=root/'state/paru';state.mkdir(parents=True,exist_ok=True)
    baseline='[[polybar]]\nurl="https://example.org/source-not-named-polybar"\ncommit="'+'a'*40+'"\n'
    (state/'devel.toml').write_text(baseline)
    try:
        for global_proxy, bases, changed in [(True,[],True),(False,['polybar'],True),(False,[],False)]:
            (root/'config/paru-tui/settings.toml').write_text('language="en"\nproxy_url="http://127.0.0.1:7890"\ngit_proxy='+str(global_proxy).lower()+'\nproxy_bases='+json.dumps(bases)+'\n')
            (root/'remote-commit').write_text(('b' if changed else 'a')*40)
            (root/'git-requests').write_text('')
            pid,fd=pty.fork()
            if pid==0: os.execve(str(BINARY),[str(BINARY)],env)
            fcntl.ioctl(fd,termios.TIOCSWINSZ,struct.pack('HHHH',36,140,0,0))
            screen=Screen();deadline=time.monotonic()+20;done=False
            try:
                while time.monotonic()<deadline:
                    if not select.select([fd],[],[],.1)[0]: continue
                    try: data=os.read(fd,65536)
                    except OSError: break
                    screen.feed(data);view=screen.text()
                    if 'Scan complete' in view:
                        assert ('latest-co' in view)==changed,view
                        if changed: assert 'polybar' in view,view
                        requests=[json.loads(line) for line in (root/'git-requests').read_text().splitlines()]
                        assert requests, 'scanner did not query Git'
                        expected='http://127.0.0.1:7890' if global_proxy or bases else None
                        assert all(r['proxy']==expected and 'ls-remote' in r['args'] for r in requests),requests
                        assert (state/'devel.toml').read_text()==baseline, 'scan advanced the installed baseline'
                        os.write(fd,b'q');done=True;break
                assert done,screen.text()
                _,status=os.waitpid(pid,0);pid=None
                assert os.waitstatus_to_exitcode(status)==0
            finally:
                if pid is not None: os.kill(pid,signal.SIGTERM);os.waitpid(pid,0)
                os.close(fd)
    finally:
        server.shutdown()
print('PASS: native navigation, local mirrors/RPC, search, proxy persistence, cancel, terminal restore, native backend query, language switching, single-AUR confirmation, details, dependency trees, and Git commit detection/routing')
