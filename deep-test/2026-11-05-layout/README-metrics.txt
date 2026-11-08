Topology metrics for the ZRemesher Plan Layout (2026-11-08, commit 2e2e257 plus the metrics task)
==================================================================================================

Scripts in this directory (plain python3, OBJ `v`/`f` records; each under 2 s on a 10.8k-quad OBJ):

  quality.py OBJ [...]            badly shaped quads: edge ratio > 3 or a corner outside 45..135 deg
  spikes.py OBJ [...]             edges over 4x the median, folded quads, collapsed faces
  badwhere.py OBJ [...] [--face]  bad quads per T-pose region; --face splits the head into cranium, eyes,
                                  nose, mouth, cheek/jaw and other (in that order, so they sum to the head)
  poles.py OBJ [...] [--plane X]  poles (valence != 4), per 1000 quads, on the plane, per region, reduction rings
  loops.py OBJ x,y,z [--also x,y,z] [--max 8]
                                  closed edge loops (Blender loop select: through valence-4 vertices only)
                                  that go round the point; --also: whether each also goes round a second point
  edgecover.py OBJ LINES.obj      fraction of hard-line length within 0.15 quad of a mesh edge, the largest
                                  distance, and the poles within 2 quads of a line. LINES.obj comes from the
                                  run: KLAY_ZR_FEATLINES=/abs/lines.obj (written in zremesh() once the
                                  feature lines are known; empty when Detect Edges is off and nothing is creased)
  badrows.py / patchbad.py        older: bad quads per 0.1 y band; bad quads per patch (planned path only)

Render sheet: bash /home/ozzy/.cache/klay-zr/sheet5.sh RESULT.obj OUT.png -> 3000 x 600 PNG in about 15 s
(whole body front, upper back, face three-quarter, hand from above, face front close-up).

Harness: `cargo test --release --bin klay zremesh::shape_tests::dump -- --ignored --nocapture` prints
'poles: N, on plane K' after every result line (K = poles with |x| under 0.05 of the mean edge length).
Hard-surface shapes: ZR_ONLY=plate,finial (2k quads each, Detect Edges on unless ZR_EDGES=0).

Targets from the reference pictures (quadremesh-ref): BODY-16 10-15 poles per 1000 quads; GENERAL-1 four
poles at the diagonals of every round feature; FACE-1/4/5 at least 5 closed loops round an eye, 4 round the
mouth with the outermost enclosing the nose; HARD-1/10 every crease a mesh edge loop, no pole within 2
cells of a crease.

Baselines at commit 2e2e257 (results under /home/ozzy/.cache/klay-zr/r1/metrics/)
-----------------------------------------------------------------------------------

T-pose 10k (tp/, body builder, ZR_K=10 ZR_EDGES=0 ZR_FOLLOW=0; identical to ~/.cache/klay-zr/tp-t7 and
baseline/tpose-10k.obj):
  quality   10800 quads, 224 badly shaped (2.1%); 0 long edges, 0 folded, 0 collapsed
  badwhere  torso 3432 q / 86 bad (2.5%), head 1688 / 38 (2.3%), arms 1355 / 10 (0.7%), legs 3184 / 28 (0.9%),
            hands 521 / 24 (4.6%), feet 620 / 38 (6.1%)
  --face    head 1688 quads, 38 bad: cranium 1332 / 24, eyes 146 / 8, nose 80 / 0, mouth 6 / 0,
            cheek/jaw 2 / 0, other 122 / 6 (the 10k face is small: eyes to mouth is four edges)
  poles     630 poles, 58.3 per 1000 quads, 2 on the plane (valence 3: 346, 5: 230, 6: 54)
            torso 206, head 144, arms 28, legs 56, hands 116, feet 80; shoulder band 206, face 71
            rings: brow/eyes y 0.785..0.842 (42), under the chin y 0.692..0.702 (8), neck base y 0.581..0.638 (84),
            collarbones y 0.420..0.540 (164, the biggest reduction zone), wrists y 0.22..0.30 (58 + 54),
            waist y 0.239..0.275 (32), hips y -0.006..0.009 (10), ankles y -0.785..-0.774 (28 + 28),
            feet y -0.88..-0.85 (16 + 16) and toes y -0.95..-0.92 (24 + 24)
  harness   'poles: 630, on plane 2'
  loops     left eye [0.023, 0.818, 0.085]: 1 closed loop (14 edges, the socket's cut border; the ring inside
            it carries the grid fill's valence-3 corners, so loop select stops there)
            mouth [0, 0.76, 0.107]: 0 closed loops (the 12-edge rim has valence-3 corners where the grid
            attaches; the only closed loops near it are rings round the neck, which loops.py rejects as not
            disc-like); so the --also nose test has nothing to report yet
            breast tip [0.111, 0.491, 0.143]: 3 closed loops of 44 edges (bump cut: border 44, 3 rings)
  sheet     tp-t7-sheet5.png (sheet5.sh, 14 s), tp-sheet.png (wt-sheet.sh)

Alien 10k (alien/, ~/.cache/klay-zr/ithrek/all.obj, ZR_VOXEL=192, body builder with joins):
  edges off  10483 quads, 896 bad (8.5%), 13 long edges (7.0x), 28 folded, 0 collapsed; 880 poles
             (83.9 per 1000), 5 on the plane; 'poles: 880, on plane 5'
  edges on   (alien-edges/, ZR_EDGES=1 KLAY_ZR_FEATLINES) 10483 quads, 1045 bad (10.0%), 60 folded,
             14 collapsed; 880 poles. lines.obj: 401 feature lines, 1426 quads of line.
  edgecover  50.8% of line length within 0.15 quad of a mesh edge, max distance 1.77 quads,
             587 poles within 2 quads of a line
  (wt-model.sh sets ZR_EDGES=0 last, so pass edges-on runs directly, not through it)

Bust 10k (bust/, ZR_VOXEL=128, planned layout, 77 s):
  11378 quads, 813 bad (7.1%), 2 long edges, 21 folded, 1 collapsed; 431 poles (37.9 per 1000), 2 on the
  plane, 8 border vertices (open at the base); 'poles: 431, on plane 1' (harness: mean edge; poles.py: median)

Plate 2k (hard/plate-new.obj, 30 x 4 x 20 cells of 0.1 with a two-step flange and three chamfered
through-holes; planned path, Detect Edges on, 4.8 s, no panic):
  3214 quads (target 2000; the plain remesher gives 1220), 811 bad (25.2%), 14 folded, 161 collapsed faces;
  177 poles (55.1 per 1000), valence 3: 89, 5: 72, 6: 15, 7: 1. The render (hard/plate-new-0.png) shows
  spikes from the collapsed faces and ragged hole rims and steps: the hard-surface baseline to beat.
  Unit test: closed, every edge in two triangles, Euler characteristic -4 (three holes).

Finial 2k (hard/finial-new.obj, a 48-meridian lathe of disc, neck, sphere and ledge with one flat cut into
the sphere at x = 0.42; planned path, Detect Edges on, 14.7 s, no panic):
  1749 quads, 8 bad (0.5%), 8 folded, 3 collapsed; 100 poles (57.2 per 1000), misaligned 6.1 deg.
  The sphere, neck and flat cut read well; the disc rim and the ledge edge are ragged (no edge loop along
  them yet). Unit test: closed, Euler characteristic 2.

Six shapes (six/): unchanged: ball 1002 q / 8 poles, cube 1014 / 8, torus 1288 / 0, vase 1729 / 14,
rook 1824 / 8, blob 2298 / 29; all planned, 0 folded.
