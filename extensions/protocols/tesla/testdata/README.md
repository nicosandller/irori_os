# Sample Tesla answers

What Tesla's Fleet API answers, for the tests: `vehicle_data`, and an energy site's
`live_status`, `site_info` and `calendar_history`.

Their shape (the field names and how they nest) comes from Home Assistant's `tesla_fleet` test
fixtures (`tests/components/tesla_fleet/fixtures` in home-assistant/core, Apache License 2.0).
The values that the tests check are Irori's own: a Model 3 parked in Tokyo, and a site dated
2026-11-10. Fields Irori doesn't read were trimmed from the car.
