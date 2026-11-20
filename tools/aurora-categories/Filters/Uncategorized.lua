-- Aurora Content Filter: shows titles with no genre metadata assigned.
GameListFilterCategories.User["Uncategorized"] = function(Content)
	return Content.GenreStr == nil or Content.GenreStr == ""
end
