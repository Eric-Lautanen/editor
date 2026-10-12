import struct

def sub(path):
    d = open(path, 'rb').read()
    pe = struct.unpack_from('<I', d, 0x3C)[0]
    assert d[pe:pe+4] == b'PE\x00\x00'
    opt = pe + 24
    s = struct.unpack_from('<H', d, opt + 68)[0]
    print(path, 'subsystem', s)

sub(r'target\debug\readitor.exe')
sub(r'target\release\readitor.exe')
sub(r'C:\Users\user\AppData\Local\Temp\contest\target\release\contest.exe')
