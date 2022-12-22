"""
# with process already running
process attach --name StarCraft
target modules dump sections StarCraft
# note offsets for __TEXT.__text
# look for first 4 bytes of target instruction as u32le
# because lldb doesn't seem to understand hex escapes in --string form of memory find
memory find --expression 0xdc889c89 0x00000001007fa3e0 0x0000000101447a46
breakpoint set --address 0x100846950
register read ebx
breakpoint command add --script-type python 1
# enter this script:
for group in frame.registers:
    if group.name == 'General Purpose Registers':
        for register in group.children:
            if register.name == 'ebx':
                print('APM:', int(register.value, 0))
return False
# DONE
continue
# it works
"""

import lldb

error = lldb.SBError()
debugger = lldb.SBDebugger.Create()
debugger.SetAsync(False)
target = debugger.CreateTarget('')
# Wait for StarCraft to start.
process = target.AttachToProcessWithName(debugger.GetListener(), 'StarCraft', True, error)
assert error.success, error.description

# Break on a library function that StarCraft will call early on,
# once it's had a chance to start running its own code first.
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
