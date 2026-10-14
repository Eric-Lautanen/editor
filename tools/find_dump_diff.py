import struct, sys

def load_bmp(path):
    with open(path, 'rb') as f:
        data = f.read()
    assert data[:2] == b'BM'
    off = struct.unpack_from('<I', data, 10)[0]
    w, h = struct.unpack_from('<ii', data, 18)
    h = -h  # top-down
    stride = w * 4
    px = data[off:]
    return w, h, px, stride

def get(px, stride, x, y):
    i = y * stride + x * 4
    return px[i], px[i+1], px[i+2]  # B, G, R

def diff_boxes(pa, pb, w, h, stride, x0, y0, x1, y1, thresh=12):
    boxes = []
    run = None
    # column ink profile
    ink_cols = []
    for x in range(x0, x1):
        c = 0
        for y in range(y0, y1):
            b1, g1, r1 = get(pa, stride, x, y)
            b2, g2, r2 = get(pb, stride, x, y)
            if abs(b1-b2) + abs(g1-g2) + abs(r1-r2) > thresh:
                c += 1
        ink_cols.append(c)
    # find clusters of columns with ink
    clusters = []
    s = None
    for i, c in enumerate(ink_cols):
        if c > 0 and s is None:
            s = i
        elif c == 0 and s is not None:
            clusters.append((x0+s, x0+i-1))
            s = None
    if s is not None:
        clusters.append((x0+s, x0+len(ink_cols)-1))
    for (cx0, cx1) in clusters:
        # row extent for this column cluster
        ys = [y for y in range(y0, y1)
              if any(abs(sum(get(pa, stride, x, y)[j]-get(pb, stride, x, y)[j] for j in range(3))) > thresh
                     for x in range(cx0, cx1+1, 2))]
        if ys:
            boxes.append((cx0, min(ys), cx1, max(ys)))
    return boxes

def main():
    d = sys.argv[1]
    a_name, b_name = sys.argv[2], sys.argv[3]
    x0, y0, x1, y1 = map(int, sys.argv[4:8])
    w, h, pa, stride = load_bmp(f'{d}\\{a_name}.bmp')
    _, _, pb, _ = load_bmp(f'{d}\\{b_name}.bmp')
    boxes = diff_boxes(pa, pb, w, h, stride, x0, y0, x1, y1)
    print(f'{a_name} vs {b_name} in ({x0},{y0})-({x1},{y1}): {len(boxes)} diff boxes')
    for b in boxes:
        print(f'  x {b[0]}..{b[2]}  y {b[1]}..{b[3]}  ({b[2]-b[0]+1}x{b[3]-b[1]+1})')

main()
