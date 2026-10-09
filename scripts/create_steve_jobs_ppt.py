from pptx import Presentation
from pptx.util import Inches, Pt
from pptx.dml.color import RGBColor
from pptx.enum.text import PP_ALIGN, MSO_ANCHOR
from pptx.enum.shapes import MSO_SHAPE
import os

OUT = os.path.expanduser('~/Desktop/Steve_Jobs_Life.pptx')
PORTRAIT = '/tmp/steve_jobs_ppt/jobs.jpg'
W, H = 13.333, 7.5
prs = Presentation()
prs.slide_width, prs.slide_height = Inches(W), Inches(H)
prs.core_properties.title = 'Steve Jobs: A Life of Vision, Reinvention & Impact'
prs.core_properties.subject = 'A concise biography of Steve Jobs'
prs.core_properties.author = 'Haarness'
prs.core_properties.keywords = 'Steve Jobs, Apple, Pixar, innovation, biography'

C = {
    'black': RGBColor(12, 12, 14), 'ink': RGBColor(24, 25, 28),
    'white': RGBColor(247, 247, 245), 'muted': RGBColor(171, 174, 180),
    'gray': RGBColor(76, 79, 87), 'line': RGBColor(48, 50, 56),
    'orange': RGBColor(244, 107, 44), 'blue': RGBColor(44, 156, 255),
    'green': RGBColor(76, 201, 130), 'yellow': RGBColor(248, 193, 64),
}
FONT = 'Aptos'


def bg(slide, color='black'):
    fill = slide.background.fill
    fill.solid(); fill.fore_color.rgb = C[color]


def rect(slide, x, y, w, h, fill, radius=False, line=None, transparency=0):
    shp = slide.shapes.add_shape(MSO_SHAPE.ROUNDED_RECTANGLE if radius else MSO_SHAPE.RECTANGLE,
                                 Inches(x), Inches(y), Inches(w), Inches(h))
    shp.fill.solid(); shp.fill.fore_color.rgb = C[fill] if isinstance(fill, str) else fill
    shp.fill.transparency = transparency
    shp.line.fill.background() if line is None else None
    if line:
        shp.line.color.rgb = C[line]; shp.line.width = Pt(1)
    return shp


def line(slide, x1, y1, x2, y2, color='line', width=1.5):
    shp = slide.shapes.add_shape(MSO_SHAPE.RECTANGLE, Inches(x1), Inches(y1), Inches(x2-x1), Inches(max(y2-y1, 0.015)))
    shp.fill.solid(); shp.fill.fore_color.rgb = C[color]; shp.line.fill.background()
    return shp


def text(slide, s, x, y, w, h, size=20, color='white', bold=False,
         font=FONT, align=PP_ALIGN.LEFT, valign=MSO_ANCHOR.TOP, margin=0, fit=False):
    box = slide.shapes.add_textbox(Inches(x), Inches(y), Inches(w), Inches(h))
    tf = box.text_frame; tf.clear(); tf.word_wrap = True
    tf.margin_left = tf.margin_right = tf.margin_top = tf.margin_bottom = Inches(margin)
    tf.vertical_anchor = valign
    p = tf.paragraphs[0]; p.alignment = align
    r = p.add_run(); r.text = s
    r.font.name = font; r.font.size = Pt(size); r.font.bold = bold
    r.font.color.rgb = C[color] if isinstance(color, str) else color
    if fit:
        tf.fit_text(font_family=font, max_size=size)
    return box


def rich_text(slide, runs, x, y, w, h, size=20, align=PP_ALIGN.LEFT, valign=MSO_ANCHOR.TOP):
    box = slide.shapes.add_textbox(Inches(x), Inches(y), Inches(w), Inches(h))
    tf = box.text_frame; tf.clear(); tf.word_wrap = True; tf.vertical_anchor = valign
    tf.margin_left = tf.margin_right = tf.margin_top = tf.margin_bottom = 0
    p = tf.paragraphs[0]; p.alignment = align
    for content, color, bold in runs:
        r = p.add_run(); r.text = content; r.font.name = FONT; r.font.size = Pt(size); r.font.bold = bold; r.font.color.rgb = C[color]
    return box


def label(slide, s, x=0.65, y=0.38, color='orange'):
    text(slide, s.upper(), x, y, 4.8, .3, 11, color, True)


def footer(slide, n):
    text(slide, f'{n:02d}', 12.25, 7.03, .45, .2, 10, 'muted', True, align=PP_ALIGN.RIGHT)
    line(slide, .65, 7.12, 11.25, 7.125, 'line')


def circle(slide, cx, cy, d, fill, txt='', tcolor='white', tsize=16):
    shp = slide.shapes.add_shape(MSO_SHAPE.OVAL, Inches(cx-d/2), Inches(cy-d/2), Inches(d), Inches(d))
    shp.fill.solid(); shp.fill.fore_color.rgb = C[fill]; shp.line.fill.background()
    if txt:
        tf = shp.text_frame; tf.clear(); tf.vertical_anchor = MSO_ANCHOR.MIDDLE
        p = tf.paragraphs[0]; p.alignment = PP_ALIGN.CENTER
        r = p.add_run(); r.text = txt; r.font.name = FONT; r.font.size = Pt(tsize); r.font.bold = True; r.font.color.rgb = C[tcolor]
    return shp


def add_title(slide, title, subtitle=None):
    text(slide, title, .65, .82, 11.8, .75, 31, 'white', True)
    if subtitle: text(slide, subtitle, .67, 1.54, 10.9, .43, 15, 'muted')


def add_notes(slide, notes):
    # Speaker-note parts created by some python-pptx versions are rejected by
    # current Keynote importers. Keep the deck broadly compatible by omitting them.
    pass

# 1 — Cover
s = prs.slides.add_slide(prs.slide_layouts[6]); bg(s)
# crop portrait to right half
s.shapes.add_picture(PORTRAIT, Inches(8.2), Inches(0), width=Inches(5.133), height=Inches(7.5))
# subtle black panel and accent
rect(s, 0, 0, 8.5, 7.5, 'black')
rect(s, 8.15, 0, .18, 7.5, 'orange')
label(s, 'Biography · 1955–2011', .72, .64)
text(s, 'STEVE\nJOBS', .72, 1.25, 6.7, 2.3, 54, 'white', True)
text(s, 'A life of vision, reinvention\n& enduring impact', .76, 3.72, 6.7, 1.05, 24, 'muted')
rich_text(s, [('“Stay hungry. ', 'white', True), ('Stay foolish.”', 'orange', True)], .76, 5.55, 6.4, .55, 21)
text(s, 'Stanford Commencement · 2005', .77, 6.18, 4.5, .3, 11, 'muted')
text(s, 'Photo: Matthew Yohe / Wikimedia Commons (CC BY-SA 3.0)', 8.52, 7.12, 4.3, .18, 8, 'white', align=PP_ALIGN.RIGHT)
add_notes(s, 'Open with the central theme: Steve Jobs repeatedly connected technology with the humanities. This presentation follows the turning points that shaped his life and legacy.')

# 2 — Timeline
s = prs.slides.add_slide(prs.slide_layouts[6]); bg(s); label(s, 'Life at a glance'); add_title(s, 'Seven turning points', 'A nonlinear path from a Silicon Valley garage to global cultural influence')
line(s, .95, 3.62, 12.25, 3.625, 'gray')
events = [
    (1.05,'1955','Born','San Francisco'), (2.85,'1976','Apple','Co-founded'), (4.55,'1985','Exit','Leaves Apple'),
    (6.3,'1986','Pixar','Acquired'), (8.0,'1997','Return','Interim CEO'), (9.75,'2007','iPhone','Introduced'), (11.55,'2011','Legacy','Dies at 56')]
colors=['yellow','orange','muted','blue','green','orange','white']
for i,(x,yr,head,sub) in enumerate(events):
    circle(s,x,3.62,.28,colors[i])
    up = i%2==0
    text(s, yr, x-.48, 2.46 if up else 4.03, .96, .3, 16, colors[i], True, align=PP_ALIGN.CENTER)
    text(s, head, x-.65, 2.82 if up else 4.39, 1.3, .3, 14, 'white', True, align=PP_ALIGN.CENTER)
    text(s, sub, x-.7, 3.12 if up else 4.69, 1.4, .3, 11, 'muted', align=PP_ALIGN.CENTER)
text(s, 'His greatest product may have been the company he rebuilt.', .95, 6.03, 11.2, .55, 23, 'white', True, align=PP_ALIGN.CENTER)
footer(s,2); add_notes(s, 'Use this slide as a map. Emphasize that Jobs’ career had two Apple chapters, with exile and Pixar in between—experiences that transformed his leadership.')

# 3 — Roots
s = prs.slides.add_slide(prs.slide_layouts[6]); bg(s); label(s, '01 · Roots'); add_title(s, 'Curiosity before credentials', 'Adoption, counterculture, calligraphy—and an early instinct for elegant systems')
items=[('1955','Adopted by Paul & Clara Jobs; raised in the Santa Clara Valley.'),('1972','Enrolls at Reed College, then drops out after one semester.'),('1974','Works at Atari; travels to India seeking spiritual insight.')]
for i,(yr,body) in enumerate(items):
    y=2.1+i*1.28
    text(s,yr,.8,y,.9,.42,18,['yellow','orange','green'][i],True)
    line(s,1.8,y+.12,2.25,y+.125,['yellow','orange','green'][i])
    text(s,body,2.5,y-0.03,5.2,.72,18,'white',i==0)
rect(s,8.35,1.98,4.1,3.8,'ink',True,line='line')
text(s,'CALLIGRAPHY',8.72,2.35,2.6,.32,12,'orange',True)
text(s,'“It was beautiful, historical, artistically subtle…”',8.72,2.93,3.25,1.15,25,'white',True)
text(s,'The classes Jobs audited at Reed later influenced the Macintosh’s typography.',8.72,4.56,3.1,.74,14,'muted')
text(s,'DESIGN LESSON',.8,6.25,1.55,.25,11,'orange',True)
text(s,'The “useless” dots often connect only in retrospect.',2.5,6.09,8.8,.55,23,'white',True)
footer(s,3); add_notes(s, 'Jobs was born February 24, 1955, and adopted by Paul and Clara Jobs. At Reed, he dropped required classes but continued auditing calligraphy—a story he later used to explain the Macintosh’s typographic sophistication.')

# 4 — Apple beginning
s=prs.slides.add_slide(prs.slide_layouts[6]); bg(s); label(s,'02 · The garage myth'); add_title(s,'Two Steves. One audacious idea.','Make personal computing accessible—and desirable.')
# product-like visual
rect(s,.75,2.05,4.8,3.7,'ink',True,line='line')
rect(s,1.18,2.48,3.95,2.2,'black',True,line='gray')
for row in range(4):
    for col in range(8):
        rect(s,1.32+col*.43,4.93+row*.15, .32,.09,'gray',True)
text(s,'APPLE I',2.03,3.08,2.2,.4,27,'green',True,align=PP_ALIGN.CENTER)
text(s,'1976',2.55,3.65,1.1,.3,13,'muted',True,align=PP_ALIGN.CENTER)
steps=[('01','Jobs sees the product','A complete experience, not just a circuit board.'),('02','Wozniak builds it','Technical brilliance powers the Apple I and II.'),('03','A company scales','Apple goes public in 1980; Jobs becomes a public icon.')]
for i,(num,head,body) in enumerate(steps):
    y=2.05+i*1.32
    circle(s,6.35,y+.28,.48,['orange','blue','green'][i],num,'black',12)
    text(s,head,6.82,y,4.8,.34,18,'white',True)
    text(s,body,6.82,y+.42,4.75,.52,14,'muted')
text(s,'Jobs’ edge: translating engineering into a compelling story.',6.05,6.18,5.8,.46,20,'orange',True)
footer(s,4); add_notes(s, 'Steve Jobs and Steve Wozniak founded Apple with Ronald Wayne in 1976. Wozniak was the engineering force; Jobs focused intensely on product, positioning, and the total customer experience.')

# 5 — Macintosh / conflict
s=prs.slides.add_slide(prs.slide_layouts[6]); bg(s); label(s,'03 · Triumph & collision'); add_title(s,'The Macintosh changed the interface','Then conflict changed Jobs.')
# split stats/cards
rect(s,.75,2.05,5.75,3.95,'white',True)
text(s,'1984',1.16,2.45,1.25,.4,27,'black',True)
text(s,'HELLO',1.16,3.23,4.75,.75,49,'black',True)
text(s,'The Macintosh popularized a graphical interface, mouse-driven navigation and expressive typography.',1.17,4.55,4.7,.8,16,'gray')
rect(s,6.85,2.05,5.75,3.95,'ink',True,line='line')
text(s,'1985',7.25,2.45,1.25,.4,27,'orange',True)
text(s,'OUT.',7.25,3.22,4.75,.75,49,'white',True)
text(s,'After a boardroom power struggle with CEO John Sculley, Jobs leaves the company he co-founded.',7.27,4.55,4.65,.8,16,'muted')
text(s,'The setback was public. The reinvention was personal.',.77,6.44,11.6,.38,19,'white',True,align=PP_ALIGN.CENTER)
footer(s,5); add_notes(s, 'The Macintosh launched in January 1984. Although commercially challenged at first, it defined modern personal-computing interaction. In 1985, Jobs lost operational control and resigned after conflict with Apple leadership.')

# 6 — Wilderness
s=prs.slides.add_slide(prs.slide_layouts[6]); bg(s); label(s,'04 · The wilderness years'); add_title(s,'NeXT sharpened the system. Pixar opened the heart.','What looked like exile became a decade of experimentation.')
rect(s,.75,2.05,5.75,3.82,'ink',True,line='line'); rect(s,6.85,2.05,5.75,3.82,'ink',True,line='line')
circle(s,1.5,2.78,.9,'black','N','white',27); text(s,'NeXT',2.12,2.46,2.5,.42,26,'white',True)
text(s,'1985–1996',2.12,2.92,1.8,.3,12,'muted',True)
text(s,'Built high-end computers and an advanced object-oriented operating system.',1.18,3.72,4.85,.7,17,'white')
text(s,'Its software foundation later became the core of Mac OS X—and today’s Apple platforms.',1.18,4.72,4.82,.74,15,'muted')
circle(s,7.65,2.78,.9,'blue','P','white',27); text(s,'Pixar',8.28,2.46,2.5,.42,26,'white',True)
text(s,'1986–2006',8.28,2.92,1.8,.3,12,'muted',True)
text(s,'Backed a studio where technology served storytelling.',7.28,3.72,4.85,.7,17,'white')
text(s,'Toy Story (1995) became the first fully computer-animated feature film; Disney bought Pixar in 2006.',7.28,4.72,4.82,.74,15,'muted')
text(s,'Technology × liberal arts',.8,6.3,11.7,.46,24,'orange',True,align=PP_ALIGN.CENTER)
footer(s,6); add_notes(s, 'Jobs founded NeXT after leaving Apple and purchased the computer-graphics group that became Pixar in 1986. NeXT’s software ultimately enabled his return, while Pixar demonstrated his ability to foster creative institutions beyond hardware.')

# 7 — Return
s=prs.slides.add_slide(prs.slide_layouts[6]); bg(s); label(s,'05 · The comeback'); add_title(s,'Return. Simplify. Rebuild.','Apple acquired NeXT in 1997—and Jobs reset the company around focus.')
# 2x2 matrix
rect(s,.8,2.08,5.55,3.9,'ink',True,line='line')
text(s,'CONSUMER',2.52,2.32,2.1,.3,12,'muted',True,align=PP_ALIGN.CENTER)
text(s,'PRO',9.0,2.32,1.2,.3,12,'muted',True,align=PP_ALIGN.CENTER)
text(s,'DESKTOP',.98,3.22,1.15,.25,11,'muted',True)
text(s,'PORTABLE',.98,4.97,1.15,.25,11,'muted',True)
products=[(2.22,2.85,'iMac','green'),(4.25,2.85,'Power Mac','blue'),(2.22,4.58,'iBook','yellow'),(4.25,4.58,'PowerBook','orange')]
for x,y,n,c in products:
    rect(s,x,y,1.7,1.1,c,True); text(s,n,x,y+.35,1.7,.3,14,'black',True,align=PP_ALIGN.CENTER)
text(s,'THE RESET',7.05,2.1,2,.3,12,'orange',True)
principles=[('01','Cut dozens of products.'),('02','Build a functional leadership team.'),('03','Control the integrated experience.'),('04','Say “no” to a thousand things.')]
for i,(n,b) in enumerate(principles):
    y=2.78+i*.79
    text(s,n,7.05,y,.45,.28,12,'orange',True)
    text(s,b,7.62,y-.02,4.45,.34,17,'white',i==3)
text(s,'Focus was not minimalism for its own sake—it was strategy.',7.05,6.02,5.1,.56,18,'muted')
footer(s,7); add_notes(s, 'Apple acquired NeXT in December 1996; Jobs soon became interim CEO. He simplified a confusing product portfolio into a clear matrix and recruited or elevated leaders who could own functions at company scale.')

# 8 — Products
s=prs.slides.add_slide(prs.slide_layouts[6]); bg(s); label(s,'06 · The product era'); add_title(s,'Four launches that reshaped daily life','A connected sequence—not isolated hits.')
products=[('1998','iMac','Reframed the PC\nas approachable','green'),('2001','iPod','Put 1,000 songs\nin your pocket','blue'),('2007','iPhone','Merged phone, web\nand music','orange'),('2010','iPad','Made touch computing\na new category','yellow')]
for i,(yr,name,body,c) in enumerate(products):
    x=.7+i*3.13
    rect(s,x,2.1,2.73,3.88,'ink',True,line='line')
    rect(s,x,2.1,2.73,.12,c,True)
    text(s,yr,x+.28,2.45,1.0,.3,13,c,True)
    text(s,name,x+.28,3.03,2.15,.52,27,'white',True)
    text(s,body,x+.28,4.02,2.15,.85,17,'muted')
    circle(s,x+2.22,5.46,.34,c,str(i+1),'black',10)
rich_text(s,[('Apple became a platform for ', 'muted', False),('devices + software + services.', 'white', True)], .75,6.35,11.8,.4,20,PP_ALIGN.CENTER)
footer(s,8); add_notes(s, 'The iMac restored momentum; the iPod and iTunes ecosystem transformed music; the iPhone redefined the smartphone; and the iPad extended multi-touch computing. Jobs’ key advantage was integration across hardware, software, and services.')

# 9 — Craft
s=prs.slides.add_slide(prs.slide_layouts[6]); bg(s); label(s,'07 · How he worked'); add_title(s,'A demanding philosophy of craft','Powerful principles—paired with real human costs.')
principles=[('FOCUS','Decide what not to do.','orange'),('END-TO-END','Own the whole experience.','blue'),('TASTE','Make technology feel humane.','green'),('STORY','Turn a launch into a narrative.','yellow')]
for i,(head,body,c) in enumerate(principles):
    x=.78+(i%2)*6.12; y=2.05+(i//2)*1.52
    rect(s,x,y,5.55,1.22,'ink',True,line='line'); rect(s,x,y,.12,1.22,c,True)
    text(s,head,x+.38,y+.25,1.75,.28,13,c,True)
    text(s,body,x+2.03,y+.23,3.0,.56,18,'white',True)
rect(s,.78,5.37,11.67,.95,'orange',True)
text(s,'THE TENSION',1.08,5.67,1.65,.25,12,'black',True)
text(s,'Intensity produced excellence—but could also wound colleagues and distort teams.',2.8,5.58,9.0,.45,17,'black',True)
text(s,'Great leadership lessons can be studied without romanticizing every behavior.',.8,6.6,11.6,.28,14,'muted',align=PP_ALIGN.CENTER)
footer(s,9); add_notes(s, 'Jobs was celebrated for focus, product taste, and storytelling. He was also known for abrasive, sometimes harmful behavior. A balanced view separates transferable product principles from conduct that should not be copied.')

# 10 — Personal / health
s=prs.slides.add_slide(prs.slide_layouts[6]); bg(s); label(s,'08 · The private arc'); add_title(s,'Family, illness & urgency','Behind the public icon was a complicated, intensely private life.')
entries=[('1991','Marries Laurene Powell; they raise three children together.','green'),('2003','Diagnosed with a rare pancreatic neuroendocrine tumor.','yellow'),('2004–09','Undergoes surgery and later a liver transplant.','orange'),('2011','Resigns as CEO in August; dies October 5 at age 56.','white')]
line(s,1.45,2.05,1.45,6.25,'gray')
for i,(yr,body,c) in enumerate(entries):
    y=2.15+i*1.02
    circle(s,1.45,y+.13,.26,c)
    text(s,yr,1.82,y-.02,1.1,.3,14,c,True)
    text(s,body,3.05,y-.07,8.7,.57,18,'white',i==3)
rect(s,8.1,5.98,4.1,.64,'ink',True,line='line')
text(s,'“Death is very likely the single best invention of Life.”',8.36,6.15,3.55,.28,12,'muted',True,align=PP_ALIGN.CENTER)
text(s,'Note: Jobs also had daughter Lisa Brennan-Jobs from an earlier relationship.',.8,6.55,6.8,.27,11,'muted')
footer(s,10); add_notes(s, 'Jobs married Laurene Powell in 1991. He had four children, including Lisa Brennan-Jobs. Diagnosed in 2003, he faced years of serious illness. His treatment choices remain debated; avoid oversimplifying the medical history.')

# 11 — Legacy
s=prs.slides.add_slide(prs.slide_layouts[6]); bg(s); label(s,'09 · Legacy'); add_title(s,'The world he helped accelerate','His influence lives in products, organizations and expectations.')
legacy=[('PRODUCTS','People now expect technology to be intuitive, beautiful and personal.','orange'),('BUSINESS','Apple’s integrated ecosystem became one of history’s most valuable models.','blue'),('CULTURE','Keynotes, design language and creative entrepreneurship became mainstream.','green')]
for i,(head,body,c) in enumerate(legacy):
    x=.78+i*4.15
    rect(s,x,2.15,3.72,3.47,'ink',True,line='line')
    circle(s,x+.55,2.75,.55,c,str(i+1),'black',14)
    text(s,head,x+.35,3.32,3.0,.3,13,c,True)
    text(s,body,x+.35,3.92,3.0,1.15,18,'white',True)
text(s,'Not a lone genius: the work depended on thousands of designers, engineers, operators and partners.',.92,6.18,11.45,.55,18,'muted',True,align=PP_ALIGN.CENTER)
footer(s,11); add_notes(s, 'Jobs’ legacy is substantial, but innovation was collective. Apple and Pixar succeeded through teams. This framing gives Jobs credit for vision and institution-building without erasing collaborators.')

# 12 — Takeaways
s=prs.slides.add_slide(prs.slide_layouts[6]); bg(s); label(s,'Closing'); add_title(s,'Five lessons from an unfinished life')
lessons=[('01','Connect the dots','Curiosity compounds.'),('02','Focus is subtraction','Strategy needs trade-offs.'),('03','Build the whole','Experience crosses boundaries.'),('04','Failure can refine','Exile can become apprenticeship.'),('05','Legacy is a team sport','Institutions outlast icons.')]
for i,(num,head,body) in enumerate(lessons):
    y=1.92+i*.89
    text(s,num,.82,y,.48,.28,12,'orange',True)
    text(s,head,1.48,y-.02,3.2,.33,18,'white',True)
    text(s,body,5.03,y,5.7,.3,16,'muted')
    line(s,.82,y+.57,12.0,y+.575,'line')
text(s,'“We’re here to put a dent in the universe.”',.9,6.57,11.5,.48,24,'orange',True,align=PP_ALIGN.CENTER)
footer(s,12); add_notes(s, 'Close by inviting discussion: Which of these lessons is most useful today—and which parts of Jobs’ style should modern leaders consciously leave behind?')

# 13 — Sources
s=prs.slides.add_slide(prs.slide_layouts[6]); bg(s); label(s,'Sources & further reading'); add_title(s,'Selected references')
refs=[
('PRIMARY','Steve Jobs, “You’ve got to find what you love,” Stanford Commencement Address (2005).'),
('BIOGRAPHY','Walter Isaacson, Steve Jobs (Simon & Schuster, 2011).'),
('APPLE','Apple Newsroom: “Steve Jobs Resigns as CEO of Apple” (Aug. 24, 2011); “Statement by Apple’s Board” (Oct. 5, 2011).'),
('HISTORY','Computer History Museum: Steve Jobs and Apple collection; Encyclopaedia Britannica, “Steve Jobs.”'),
('PIXAR','Pixar Animation Studios, company history; The Walt Disney Company, Pixar acquisition announcement (2006).'),
('IMAGE','Matthew Yohe, “Steve Jobs Headshot 2010,” Wikimedia Commons, CC BY-SA 3.0.')]
for i,(tag,body) in enumerate(refs):
    y=1.85+i*.76
    text(s,tag,.82,y,1.15,.25,11,['orange','blue','green','yellow','orange','blue'][i],True)
    text(s,body,2.15,y-.04,9.9,.5,14,'white',i==0)
text(s,'Dates and claims cross-checked against the sources above. Accessed August 2025.',.82,6.63,10.4,.28,11,'muted')
footer(s,13); add_notes(s, 'These are the principal sources used for this concise overview. For deeper treatment, start with Jobs’ 2005 Stanford speech and Walter Isaacson’s authorized biography.')

# remove default first slide if any (there is none; all added intentionally)
prs.save(OUT)
print(OUT)
print('slides:', len(prs.slides))
