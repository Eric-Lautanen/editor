import sys

path = sys.argv[1]
depth = 0
line_no = 1
stack = []
with open(path, encoding='utf-8') as f:
    text = f.read()
in_str = None
prev = ''
for i, ch in enumerate(text):
    if ch == '\n':
        line_no += 1
    if in_str:
        if ch == in_str and prev != '\\':
            in_str = None
        prev = ch
        continue
    if ch in '"\'':
        in_str = ch
    elif ch == '/' and prev == '/':
        # line comment: skip to newline
        j = text.find('\n', i)
        # handled by continuing; crude but ok
    elif ch == '{':
        stack.append(line_no)
    elif ch == '}':
        if stack:
            stack.pop()
        else:
            print(f'extra }} at line {line_no}')
    prev = ch
print('unclosed { opened at lines:', stack[-5:] if stack else 'none')
