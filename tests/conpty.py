"""Minimal Windows ConPTY harness using only the Python standard library."""
import ctypes as c
from ctypes import wintypes as w
import subprocess
import threading
import time

k=c.WinDLL("kernel32",use_last_error=True)
class COORD(c.Structure): _fields_=[("X",c.c_short),("Y",c.c_short)]
class SI(c.Structure):
    _fields_=[("cb",w.DWORD),("reserved",w.LPWSTR),("desktop",w.LPWSTR),("title",w.LPWSTR),
              ("x",w.DWORD),("y",w.DWORD),("xs",w.DWORD),("ys",w.DWORD),("xc",w.DWORD),("yc",w.DWORD),
              ("fill",w.DWORD),("flags",w.DWORD),("show",w.WORD),("reserved2",w.WORD),("bytes",c.POINTER(c.c_byte)),
              ("input",w.HANDLE),("output",w.HANDLE),("error",w.HANDLE)]
class SIX(c.Structure): _fields_=[("si",SI),("attributes",c.c_void_p)]
class PI(c.Structure): _fields_=[("process",w.HANDLE),("thread",w.HANDLE),("pid",w.DWORD),("tid",w.DWORD)]
k.CreatePipe.argtypes=[c.POINTER(w.HANDLE),c.POINTER(w.HANDLE),c.c_void_p,w.DWORD]
k.CreatePseudoConsole.argtypes=[COORD,w.HANDLE,w.HANDLE,w.DWORD,c.POINTER(w.HANDLE)]
k.CreatePseudoConsole.restype=c.c_long
k.ClosePseudoConsole.argtypes=[w.HANDLE]
k.InitializeProcThreadAttributeList.argtypes=[c.c_void_p,w.DWORD,w.DWORD,c.POINTER(c.c_size_t)]
k.UpdateProcThreadAttribute.argtypes=[c.c_void_p,w.DWORD,c.c_size_t,c.c_void_p,c.c_size_t,c.c_void_p,c.c_void_p]
k.DeleteProcThreadAttributeList.argtypes=[c.c_void_p]
k.CreateProcessW.argtypes=[w.LPCWSTR,w.LPWSTR,c.c_void_p,c.c_void_p,w.BOOL,w.DWORD,c.c_void_p,w.LPCWSTR,c.POINTER(SI),c.POINTER(PI)]
k.ReadFile.argtypes=[w.HANDLE,c.c_void_p,w.DWORD,c.POINTER(w.DWORD),c.c_void_p]
k.WriteFile.argtypes=k.ReadFile.argtypes
k.CloseHandle.argtypes=[w.HANDLE]
k.WaitForSingleObject.argtypes=[w.HANDLE,w.DWORD]
k.GetExitCodeProcess.argtypes=[w.HANDLE,c.POINTER(w.DWORD)]
k.TerminateProcess.argtypes=[w.HANDLE,w.UINT]
k.GetStdHandle.argtypes=[w.DWORD]
k.GetStdHandle.restype=w.HANDLE
k.SetStdHandle.argtypes=[w.DWORD,w.HANDLE]
def check(result):
    if not result: raise c.WinError(c.get_last_error())

class Terminal:
    def __init__(self,command,env):
        incoming,outgoing=w.HANDLE(),w.HANDLE()
        self.input,self.output=w.HANDLE(),w.HANDLE()
        check(k.CreatePipe(c.byref(incoming),c.byref(self.input),None,0))
        check(k.CreatePipe(c.byref(self.output),c.byref(outgoing),None,0))
        self.console=w.HANDLE()
        status=k.CreatePseudoConsole(COORD(160,45),incoming,outgoing,0,c.byref(self.console))
        if status: raise RuntimeError(f"CreatePseudoConsole HRESULT {status:x}")
        self.bytes=bytearray();self.cursor=0
        self.reader=threading.Thread(target=self._read,daemon=True);self.reader.start()
        size=c.c_size_t();k.InitializeProcThreadAttributeList(None,1,0,c.byref(size))
        attrs=c.create_string_buffer(size.value)
        check(k.InitializeProcThreadAttributeList(attrs,1,0,c.byref(size)))
        check(k.UpdateProcThreadAttribute(attrs,0,0x20016,self.console,c.sizeof(w.HANDLE),None,None))
        startup=SIX();startup.si.cb=c.sizeof(startup);startup.attributes=c.cast(attrs,c.c_void_p)
        block=c.create_unicode_buffer("\0".join(f"{key}={value}" for key,value in sorted(env.items()))+"\0\0")
        self.process=PI()
        # A redirected test host otherwise lends its std handles to the child.
        # Clear only this host process's table while Windows initializes ConPTY handles.
        std=[(n,k.GetStdHandle(n)) for n in (-10 & 0xffffffff,-11 & 0xffffffff,-12 & 0xffffffff)]
        try:
            for n,_ in std: check(k.SetStdHandle(n,None))
            check(k.CreateProcessW(command[0],c.create_unicode_buffer(subprocess.list2cmdline(command)),None,None,False,0x80000|0x400,block,None,c.cast(c.byref(startup),c.POINTER(SI)),c.byref(self.process)))
        finally:
            for n,handle in std: k.SetStdHandle(n,handle)
        k.CloseHandle(incoming);k.CloseHandle(outgoing)
        k.DeleteProcThreadAttributeList(attrs);k.CloseHandle(self.process.thread)
    def _read(self):
        buf=c.create_string_buffer(4096);size=w.DWORD()
        while k.ReadFile(self.output,buf,len(buf),c.byref(size),None) and size.value: self.bytes.extend(buf.raw[:size.value])
    @property
    def text(self): return bytes(self.bytes).decode("utf-8",errors="replace")
    def expect(self,value,timeout=8):
        end=time.monotonic()+timeout
        while time.monotonic()<end:
            text=self.text; index=text.find(value,self.cursor)
            if index>=0: self.cursor=index+len(value);return text
            time.sleep(.025)
        raise AssertionError(f"terminal did not contain {value!r}: {self.text}")
    def send(self,value):
        data=value.encode("utf-8");size=w.DWORD()
        check(k.WriteFile(self.input,data,len(data),c.byref(size),None))
    def wait(self,timeout=10):
        if k.WaitForSingleObject(self.process.process,int(timeout*1000))!=0: raise TimeoutError(self.text)
        code=w.DWORD();check(k.GetExitCodeProcess(self.process.process,c.byref(code)));return code.value
    def __enter__(self): return self
    def __exit__(self,*_):
        if k.WaitForSingleObject(self.process.process,0)!=0: k.TerminateProcess(self.process.process,99)
        k.WaitForSingleObject(self.process.process,5000)
        k.CloseHandle(self.input);k.ClosePseudoConsole(self.console)
        k.CloseHandle(self.output);k.CloseHandle(self.process.process)
