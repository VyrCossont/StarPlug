# Implementation notes

## `lldb` backend

This `lldb` script is the rough equivalent of the Python script that StarPlug uses to instrument StarCraft.

```lldb
# with process already running
process attach --name StarCraft

# get offsets for __TEXT.__text
target modules dump sections StarCraft

# use offsets to look for first 4 bytes of target instruction as u32le
#   --expression and 4 bytes are usd because lldb doesn't seem to understand hex escapes in --string form of memory find
#   might be an lldb bug: https://stackoverflow.com/a/33114721
memory find --expression 0xdc889c89 0x00000001007fa3e0 0x0000000101447a46
breakpoint set --address 0x100846950

# read the register and make sure it's what your APM counter is showing
register read ebx

# add a breakpoint script to print it and continue
breakpoint command add --script-type python 1
# enter this script:
for group in frame.registers:
    if group.name == 'General Purpose Registers':
        for register in group.children:
            if register.name == 'ebx':
                print('APM:', int(register.value, 0))
return False
# type DONE to end the script entry

# resume running StarCraft
continue
# it works
```
