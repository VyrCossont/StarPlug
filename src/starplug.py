"""
StarPlug instrumentation script for StarCraft: Remastered on macOS.
Expects to be told whether StarCraft is already running in the `STARCRAFT_IS_RUNNING` environment variable.
"""

import os

import lldb

error = lldb.SBError()
debugger = lldb.SBDebugger.Create()
debugger.SetAsync(False)
target = debugger.CreateTarget('')

starcraft_pid_text = os.getenv('STARCRAFT_PID')
starcraft_pid = int(starcraft_pid_text) if starcraft_pid_text else None
if starcraft_pid is not None:
    # Attach to an already running StarCraft process.
    # Assume it's been running long enough to unpack.
    process = target.AttachToProcessWithID(debugger.GetListener(), starcraft_pid, error)
    assert error.success, error.description
else:
    # Wait for StarCraft to start.
    process = target.AttachToProcessWithName(debugger.GetListener(), 'StarCraft', True, error)
    assert error.success, error.description

    # Break on a library function that StarCraft will call once early on,
    # once it's had a chance to start running its own code first.
    # Necessary because we don't know where StarCraft keeps its `main`.
    init_bp = target.BreakpointCreateByName('CGBitmapContextCreate')
    init_bp.SetOneShot(True)
    error = process.Continue()
    assert error.success, error.description

# Find an instruction where we know the APM has recently been calculated.
# We can only do this once StarCraft proper has started running and unpacked itself.
executable_module = target.FindModule(target.executable)
code_section = executable_module.FindSection('__text')
code_start = code_section.addr.GetLoadAddress(target)
code_bytes = process.ReadMemory(code_start, code_section.size, error)
assert error.success, error.description
# Soon after the `cvttss2si %xmm0, %rbx` that turns the float APM into an int for display,
# there is a `movl %ebx, 0xdc(%rax, %rcx, 4)` that we can break on.
offset_from_code_start = code_bytes.index(b'\x89\x9C\x88\xDC\x00\x00\x00')

# Break on the instruction. The APM is in EBX when it runs, so print that and continue.
bp = target.BreakpointCreateByAddress(code_start + offset_from_code_start)
error = bp.SetScriptCallbackBody("""\
for group in frame.registers:
    if group.name == 'General Purpose Registers':
        for register in group.children:
            if register.name == 'ebx':
                print('APM:', int(register.value, 0))
return False
""")
assert error.success, error.description

error = process.Continue()
assert error.success, error.description
