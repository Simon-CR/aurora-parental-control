scriptTitle = "Genre QuickView Installer"
scriptAuthor = "Simon-CR / aurora-parental-control"
scriptVersion = 1
scriptDescription = "Creates QuickView tabs by genre (plus Uncategorized) and shows genre counts"
scriptPermissions = { "sql" }

local GENRES = {
	{ name = "Action & Adventure", mask = 0x0002, filter = "Genres.Action & Adventure", icon = "GENRE_ACTION" },
	{ name = "Classics",           mask = 0x1000, filter = "Genres.Classics",           icon = "GENRE_CLASSICS" },
	{ name = "Shooter",            mask = 0x0100, filter = "Genres.Shooter",            icon = "GENRE_SHOOTER" },
	{ name = "Puzzle & Trivia",    mask = 0x2000, filter = "Genres.Puzzle & Trivia",    icon = "GENRE_PUZZLE" },
	{ name = "Racing & Flying",    mask = 0x0040, filter = "Genres.Racing & Flying",    icon = "GENRE_RACING" },
	{ name = "Family",             mask = 0x0004, filter = "Genres.Family",             icon = "GENRE_FAMILY" },
	{ name = "Platformer",         mask = 0x0020, filter = "Genres.Platformer",         icon = "GENRE_PLATFORMER" },
	{ name = "Sports & Recreation",mask = 0x0400, filter = "Genres.Sports & Recreation",icon = "GENRE_SPORTS" },
	{ name = "Strategy & Sim",     mask = 0x0200, filter = "Genres.Strategy & Simulation", icon = "GENRE_STRATEGY" },
	{ name = "Fighting",           mask = 0x0008, filter = "Genres.Fighting",           icon = "GENRE_FIGHTING" },
	{ name = "Role Playing",       mask = 0x0080, filter = "Genres.Role Playing",       icon = "GENRE_RPG" },
	{ name = "Card & Board",       mask = 0x0800, filter = "Genres.Card & Board",       icon = "GENRE_CARDS" },
	{ name = "Other",              mask = 0x0001, filter = "Genres.Other",              icon = "GENRE_OTHER" },
	{ name = "Music",              mask = 0x0010, filter = "Genres.Music",              icon = "GENRE_MUSIC" },
}

local function sqlEscape(val)
	return string.gsub(val, "'", "''")
end

local function fetchGenreCounts()
	local counts = {}
	for _, g in ipairs(GENRES) do
		local sql = "SELECT COUNT(*) AS Cnt FROM ContentItems WHERE (GenreFlag & " .. g.mask .. ") != 0;"
		local rows = Sql.ExecuteFetchRows(sql)
		local cnt = 0
		if rows ~= nil and rows[1] ~= nil and rows[1].Cnt ~= nil then
			cnt = tonumber(rows[1].Cnt) or 0
		end
		counts[g.filter] = cnt
	end

	local uncatRows = Sql.ExecuteFetchRows("SELECT COUNT(*) AS Cnt FROM ContentItems WHERE IFNULL(GenreFlag, 0) = 0;")
	local uncat = 0
	if uncatRows ~= nil and uncatRows[1] ~= nil and uncatRows[1].Cnt ~= nil then
		uncat = tonumber(uncatRows[1].Cnt) or 0
	end
	return counts, uncat
end

local function ensureQuickView(displayName, filterMethod, orderIdx, iconHash)
	local existing = Sql.ExecuteFetchRows(
		"SELECT Id FROM Config.QuickViews WHERE FilterMethod = '" .. sqlEscape(filterMethod) .. "' LIMIT 1;"
	)
	if existing ~= nil and existing[1] ~= nil then
		return false
	end
	local q = string.format(
		"INSERT INTO Config.QuickViews (DisplayName, SortMethod, FilterMethod, Flags, CreatorXUID, OrderIndex, IconHash) VALUES ('%s', 'Title Name', '%s', 2, NULL, %d, '%s');",
		sqlEscape(displayName),
		sqlEscape(filterMethod),
		orderIdx,
		sqlEscape(iconHash)
	)
	return Sql.Execute(q)
end

local function nextOrderIndex()
	local rows = Sql.ExecuteFetchRows("SELECT IFNULL(MAX(OrderIndex), 0) AS MaxIdx FROM Config.QuickViews;")
	if rows ~= nil and rows[1] ~= nil and rows[1].MaxIdx ~= nil then
		return (tonumber(rows[1].MaxIdx) or 0) + 1
	end
	return 10
end

local function installViews(minGames)
	local okProbe, probe = pcall(function()
		return Sql.ExecuteFetchRows("SELECT COUNT(*) AS Cnt FROM Config.QuickViews;")
	end)
	if not okProbe or probe == nil then
		Script.ShowMessageBox(
			"QuickView Install Unavailable",
			"Aurora's Lua runtime did not expose Config.QuickViews directly.\nYou can still press B -> Filter -> Genres (or User -> Uncategorized).",
			"OK"
		)
		return
	end

	local counts, uncat = fetchGenreCounts()
	local orderIdx = nextOrderIndex()
	local added = 0

	for _, g in ipairs(GENRES) do
		local cnt = counts[g.filter] or 0
		if cnt >= minGames then
			if ensureQuickView(g.name, g.filter, orderIdx, g.icon) then
				added = added + 1
				orderIdx = orderIdx + 1
			end
		end
	end

	if uncat > 0 then
		if ensureQuickView("Uncategorized", "User.Uncategorized", orderIdx, "GENRE_UNCAT") then
			added = added + 1
		end
	end

	Script.ShowNotification(added .. " Genre QuickViews installed")
	if added > 0 then
		local res = Script.ShowMessageBox(
			"Genre QuickViews Added",
			added .. " genre QuickViews were added to Aurora.\nRestart Aurora now so RB/LB tabs appear?",
			"Restart Now",
			"Later"
		)
		if res ~= nil and res.Canceled ~= true and res.Button == 1 then
			Aurora.Restart()
		end
	end
end

local function removeViews()
	local okCall, ok = pcall(function()
		return Sql.Execute("DELETE FROM Config.QuickViews WHERE FilterMethod LIKE 'Genres.%' OR FilterMethod = 'User.Uncategorized';")
	end)
	if not okCall or not ok then
		Script.ShowNotification("Could not modify Config.QuickViews")
		return
	end
	Script.ShowNotification("Removed Genre QuickViews")
	local res = Script.ShowMessageBox("Genre QuickViews Removed", "Restart Aurora now to refresh QuickView tabs?", "Restart Now", "Later")
	if res ~= nil and res.Canceled ~= true and res.Button == 1 then
		Aurora.Restart()
	end
end

local function showBreakdown()
	local counts, uncat = fetchGenreCounts()
	local msg = "Library Breakdown by Genre:\n\n"
	for _, g in ipairs(GENRES) do
		local c = counts[g.filter] or 0
		if c > 0 then
			msg = msg .. g.name .. ": " .. c .. "\n"
		end
	end
	msg = msg .. "Uncategorized: " .. uncat
	Script.ShowMessageBox("Aurora Genre Counts", msg, "OK")
end

function main()
	local menu = {
		"Install All Genre QuickViews (+ Uncategorized)",
		"Install Top Genres (>= 15 games)",
		"View Genre Breakdown",
		"Remove Added Genre QuickViews",
	}
	local choice = Script.ShowPopupList("Genre QuickView Manager", "No options", menu)
	if choice == nil or choice.Canceled == true or choice.Selected == nil then
		return
	end
	local key = choice.Selected.Key
	if key == 1 then
		installViews(1)
	elseif key == 2 then
		installViews(15)
	elseif key == 3 then
		showBreakdown()
	elseif key == 4 then
		removeViews()
	end
end
