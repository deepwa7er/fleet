-- Seed the standard classic men's wardrobe as the starting scaffold. Runs once
-- on a fresh database; targets are how many pieces to own in each category, and
-- the note is short guidance tuned for a tall, lean build. These are ordinary
-- rows — rename, retarget, or delete them freely from the UI afterward.

INSERT INTO categories (name, note, target_count, sort_order, created_at) VALUES
  ('Outerwear',           'Navy or camel topcoat, a field/chore jacket, a weatherproof shell. A tall frame carries a longer coat well.', 3, 10, '2026-06-30T00:00:00Z'),
  ('Suits & Tailoring',   'Navy first, then charcoal. Flat-front trousers, minimal break, tailored to a lean frame.',                     2, 20, '2026-06-30T00:00:00Z'),
  ('Blazers & Sport Coats','Navy hopsack blazer, then an odd jacket in tweed or check for texture.',                                    2, 30, '2026-06-30T00:00:00Z'),
  ('Dress Shirts',        'White and light-blue poplin or twill; spread or semi-spread collar. Slim, not tight.',                       4, 40, '2026-06-30T00:00:00Z'),
  ('Casual Shirts',       'Oxford-cloth button-downs (white, blue), flannel for cold, linen for summer.',                               5, 50, '2026-06-30T00:00:00Z'),
  ('Tees & Polos',        'Heavyweight crew tees in white/navy/grey; a knit or pique polo. Fitted through the body.',                    6, 60, '2026-06-30T00:00:00Z'),
  ('Knitwear',            'Merino crewneck, lambswool, a shawl-collar cardigan. Navy, grey, oatmeal.',                                  4, 70, '2026-06-30T00:00:00Z'),
  ('Trousers',            'Chinos in stone and olive; grey wool trousers. Slim-straight suits your height.',                            4, 80, '2026-06-30T00:00:00Z'),
  ('Denim',               'One dark rinse, one mid-wash. Slim-straight, minimal distressing.',                                          2, 90, '2026-06-30T00:00:00Z'),
  ('Shorts',              '5-7 inch inseam chino shorts in stone and navy.',                                                            2, 100, '2026-06-30T00:00:00Z'),
  ('Footwear',            'Brown derbies or oxfords, loafers, suede chukkas, white leather sneakers, one boot.',                        5, 110, '2026-06-30T00:00:00Z'),
  ('Accessories',         'Brown and black leather belts to match your shoes, a few ties, one good watch, quality socks, sunglasses.',  6, 120, '2026-06-30T00:00:00Z'),
  ('Basics & Underwear',  'Undershirts, boxer briefs, no-show and dress socks. Replace on a schedule.',                                 4, 130, '2026-06-30T00:00:00Z');
