import math
import sys

# Offline contrast verifier: proves the committed OKLCH role tokens (tokens.css,
# identity "bench") clear their WCAG bars in BOTH themes, that severity ramps stay lightness-separated,
# and that the brand accent is hue-separated from every severity color. Cross-checked by the TS port in
# web/src/ui/accentContrast.ts (accentContrast.test.ts). Run automatically in scripts/ci.sh; exits
# non-zero when TOTAL FAILS > 0 so CI blocks any contrast regression.


def lum(L,C,H):
    a=C*math.cos(math.radians(H)); b=C*math.sin(math.radians(H))
    l=(L+0.3963377774*a+0.2158037573*b)**3
    m=(L-0.1055613458*a-0.0638541728*b)**3
    s=(L-0.0894841775*a-1.2914855480*b)**3
    r=4.0767416621*l-3.3077115913*m+0.2309699292*s
    g=-1.2684380046*l+2.6097574011*m-0.3413193965*s
    bb=-0.0041960863*l-0.7034186147*m+1.7076147010*s
    return 0.2126*max(0,min(1,r))+0.7152*max(0,min(1,g))+0.0722*max(0,min(1,bb))
def cr(fg,bg):
    a,b=lum(*fg),lum(*bg); h,lo=max(a,b),min(a,b); return (h+0.05)/(lo+0.05)

# identity -> mode -> token -> (L,C,H)
P={
 "bench":{
  "dark":{
   "bg":(0.175,0.014,55),"surface":(0.215,0.016,55),"surface2":(0.260,0.017,55),
   "inset":(0.150,0.012,55),"border":(0.360,0.018,55),"border_strong":(0.520,0.020,55),
   "text":(0.930,0.014,60),"muted":(0.735,0.016,58),"faint":(0.680,0.016,58),
   "data_ink":(0.800,0.020,58),
   "cat1":(0.72,0.14,205),"cat2":(0.80,0.13,290),"cat3":(0.74,0.15,330),
   "cat4":(0.82,0.13,130),"cat5":(0.86,0.12,95),"cat6":(0.68,0.14,45),
   "accent":(0.720,0.120,262),"accent_text":(0.760,0.110,262),"accent_weak":(0.270,0.045,262),
   "brass":(0.710,0.097,78),
   "on_accent":(0.180,0.030,262),"ok":(0.860,0.100,165),"warn":(0.760,0.130,62),
   "high":(0.660,0.190,25),"surface_raised":(0.260,0.017,55),"focus_ring":(0.720,0.120,262),
  },
  "light":{
   "bg":(0.945,0.018,50),"surface":(0.978,0.014,55),"surface2":(0.955,0.016,55),
   "inset":(0.945,0.014,55),"border":(0.820,0.020,50),"border_strong":(0.560,0.022,52),
   "text":(0.255,0.020,55),"muted":(0.470,0.020,55),"faint":(0.490,0.020,55),
   "data_ink":(0.420,0.022,55),
   "cat1":(0.50,0.14,205),"cat2":(0.48,0.15,290),"cat3":(0.50,0.15,330),
   "cat4":(0.50,0.13,130),"cat5":(0.52,0.13,95),"cat6":(0.46,0.15,45),
   "accent":(0.470,0.130,258),"accent_text":(0.430,0.130,258),"accent_weak":(0.930,0.040,255),
   "brass":(0.493,0.079,78),
   "on_accent":(0.980,0.015,55),"ok":(0.500,0.110,160),"warn":(0.400,0.130,60),
   "high":(0.300,0.190,28),"surface_raised":(0.955,0.016,55),"focus_ring":(0.470,0.130,258),
  },
 },
}
fails=0
for ident,modes in P.items():
  for mode,t in modes.items():
    surf=t["surface"]; bg=t["bg"]
    def ck(fg,bgc,need,label):
      global fails
      r=cr(fg,bgc)
      if r<need: fails+=1; print(f"  FAIL {r:4.2f} (<{need}) {ident}/{mode} {label}")
    def warn(fg,bgc,need,label):
      r=cr(fg,bgc)
      if r<need: print(f"  WARN {r:4.2f} (<{need}) {ident}/{mode} {label}")
    def ck_gap(gap,need,label):
      global fails
      if gap + 1e-9 < need:
        fails+=1; print(f"  FAIL {gap:4.2f} (<{need}) {ident}/{mode} {label}")
    ck(t["text"],bg,4.5,"text/bg"); ck(t["text"],surf,4.5,"text/surf")
    ck(t["muted"],bg,4.5,"muted/bg"); ck(t["muted"],surf,4.5,"muted/surf")
    ck(t["faint"],bg,4.5,"faint/bg"); ck(t["faint"],surf,4.5,"faint/surf")
    ck(t["accent_text"],surf,4.5,"accent_text/surf")
    ck(t["accent"],surf,3.0,"accent/surf(large)")
    ck(t["on_accent"],t["accent"],4.5,"on_accent/accent")
    ck(t["ok"],surf,4.5,"ok/surf"); ck(t["warn"],surf,4.5,"warn/surf"); ck(t["high"],surf,4.5,"high/surf")
    ck(t["ok"],bg,4.5,"ok/bg"); ck(t["warn"],bg,4.5,"warn/bg"); ck(t["high"],bg,4.5,"high/bg")
    ck_gap(t["ok"][0]-t["warn"][0],0.10,"ok-warn L gap")
    ck_gap(t["warn"][0]-t["high"][0],0.10,"warn-high L gap")
    ck_gap(t["ok"][0]-t["high"][0],0.20,"ok-high L gap")
    ck(t["border_strong"],bg,3.0,"border_strong/bg")  # load-bearing rules (masthead/total) — WCAG non-text 3:1
    ck(t["brass"],bg,3.0,"brass/bg")  # resting current-location markers — WCAG non-text 3:1
    ck(t["brass"],surf,3.0,"brass/surf")  # the same marker remains distinct over a hovered row/tab
    ck(t["brass"],t["surface2"],3.0,"brass/surf2")  # transient selected-row edge markers
    ck(t["data_ink"],bg,3.0,"data_ink/bg")  # single-series data marks (bars/lines/dots) — WCAG non-text 3:1
    ck(t["data_ink"],surf,3.0,"data_ink/surf")
    for i in range(1,7):  # categorical ink marks (composition/flame/anatomy/treemap/trend) — WCAG non-text 3:1
      ck(t[f"cat{i}"],bg,3.0,f"cat{i}/bg"); ck(t[f"cat{i}"],surf,3.0,f"cat{i}/surf")
    # Brand accent must sit clearly off every severity hue (>=95 deg circular) so "brand" can never read
    # as "warning" — the exact collision the old brass (78 vs warn 55) suffered.
    def hue_dist(h1,h2):
      d=abs(h1-h2)%360; return min(d,360-d)
    for sv in ("ok","warn","high"):
      hd=hue_dist(t["accent"][2],t[sv][2])
      if hd + 1e-9 < 95.0:
        fails+=1; print(f"  FAIL {hd:5.1f} (<95) {ident}/{mode} accent-{sv} hue distance")
print(f"TOTAL FAILS: {fails}")
# Non-zero exit so scripts/ci.sh fails the build on any contrast regression.
sys.exit(1 if fails else 0)
