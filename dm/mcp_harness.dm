// byond-mcp test harness.
// Injected by byond-mcp into <name>.mcp.dmb builds only. It lets whoever holds the
// launch token call any proc in the world, so it must never ship in a normal build.
#ifndef MCP_HARNESS
#error mcp_harness.dm is injected by byond-mcp and must not be part of a normal build
#endif

/// Replies longer than this are handed over through a file instead of the topic packet (u16 length).
#define MCP_INLINE_LIMIT 48000
/// Lists are cut after this many entries when encoded.
#define MCP_LIST_LIMIT 300
/// Whether a player's mob is a character rather than a lobby placeholder.
#ifdef GAME_STATE_PLAYING
#define MCP_HAS_BODY(player_mob) istype(player_mob, /mob/living)
#else
#define MCP_HAS_BODY(player_mob) (!isnull(player_mob))
#endif
/// Whether a client has finished logging in. Touching its mob before that derails its login.
#ifdef VALIDATE_CLIENT_INITIALIZATION
#define MCP_CLIENT_READY(player) VALIDATE_CLIENT_INITIALIZATION(player)
#else
#define MCP_CLIENT_READY(player) TRUE
#endif

var/global/list/mcp_jobs = list()
var/global/mcp_job_seq = 0
var/global/mcp_reply_seq = 0

/world/Topic(T, addr, master, key)
	if(findtextEx(T, "mcp_harness=1"))
		return mcp_handle_topic(params2list(T))
	return ..()

/proc/mcp_handle_topic(list/input)
	var/token = world.params["mcp_token"]
	if(!token || input["token"] != token)
		return json_encode(list("ok" = 0, "error" = "bad token"))
	// A world with nobody connected stops its clock, which would freeze every sleeping proc a test calls.
	world.sleep_offline = FALSE
	var/datum/mcp_job/job
	try
		var/raw = input["data"]
		if(input["file"])
			raw = file2text(input["file"])
			fdel(input["file"])
		var/list/request = json_decode(raw)
		if(request["op"] == "job")
			job = mcp_jobs["[request["id"]]"]
			if(!job)
				return json_encode(list("ok" = 0, "error" = "unknown job [request["id"]]"))
		else
			job = new
			job.id = ++mcp_job_seq
			mcp_jobs["[job.id]"] = job
			job.execute(request)
	catch(var/thrown)
		return json_encode(list("ok" = 0, "error" = mcp_describe_exception(thrown)))
	return job.reply()

/proc/mcp_describe_exception(thrown)
	if(istype(thrown, /exception))
		var/exception/ex = thrown
		return "[ex.name] at [ex.file]:[ex.line][ex.desc ? "\n[ex.desc]" : ""]"
	return "[thrown]"

/// One harness request. Runs detached so a sleeping proc cannot stall world/Topic.
/datum/mcp_job
	var/id
	var/done = FALSE
	var/ok = FALSE
	var/result
	var/error

/datum/mcp_job/proc/execute(list/request)
	set waitfor = FALSE
	try
		var/depth = isnum(request["depth"]) ? request["depth"] : 2
		result = mcp_encode(mcp_run_op(request), depth)
		ok = TRUE
	catch(var/thrown)
		error = mcp_describe_exception(thrown)
	done = TRUE

/datum/mcp_job/proc/reply()
	if(!done)
		return json_encode(list("ok" = 1, "running" = 1, "job" = id))
	mcp_jobs -= "[id]"
	var/reply_text = json_encode(ok ? list("ok" = 1, "result" = result) : list("ok" = 0, "error" = error))
	if(length(reply_text) <= MCP_INLINE_LIMIT)
		return reply_text
	var/reply_path = "data/mcp_harness/reply_[++mcp_reply_seq].json"
	fdel(reply_path)
	text2file(reply_text, reply_path)
	return json_encode(list("ok" = 1, "file" = reply_path))

/proc/mcp_client(key)
	var/wanted = ckey(key)
	for(var/client/candidate)
		if(candidate.ckey == wanted)
			return candidate
	throw EXCEPTION("no connected client with ckey [wanted]")

/proc/mcp_global(name)
#ifdef GLOBAL_PROTECT
	if(name in GLOB.vars)
		return GLOB.vars[name]
#endif
	return global.vars[name]

/// Turns decoded JSON into live values: {"$ref": ...}, {"$mob": ckey} and friends become objects.
/proc/mcp_resolve(value)
	if(!islist(value))
		return value
	var/list/input = value
	if(length(input))
		var/marker = input[1]
		if(istext(marker) && length(marker) && marker[1] == "$")
			var/argument = input[marker]
			switch(marker)
				if("$ref")
					. = locate(argument)
					if(isnull(.))
						throw EXCEPTION("nothing has ref [argument]")
					return
				if("$path")
					. = text2path(argument)
					if(isnull(.))
						throw EXCEPTION("no such type path [argument]")
					return
				if("$client")
					return mcp_client(argument)
				if("$mob")
					var/client/owner = mcp_client(argument)
					return owner.mob
				if("$global")
					return mcp_global(argument)
				if("$turf")
					var/list/coords = argument
					. = locate(coords[1], coords[2], coords[3])
					if(isnull(.))
						throw EXCEPTION("no turf at [coords[1]],[coords[2]],[coords[3]]")
					return
				if("$world")
					return world
			throw EXCEPTION("unknown reference marker [marker]")
	var/list/out = list()
	for(var/i in 1 to length(input))
		var/entry = input[i]
		var/assoc_value = istext(entry) ? input[entry] : null
		if(isnull(assoc_value))
			out[++out.len] = mcp_resolve(entry)
		else
			out[entry] = mcp_resolve(assoc_value)
	return out

/// Name/value pairs whose values may be null, which an associative list cannot tell apart from a plain entry.
/datum/mcp_record
	var/list/names = list()
	var/list/values = list()

/// Turns live values into JSON-safe ones. Lists consume depth; objects become {"$ref": ...} summaries.
/proc/mcp_encode(value, depth)
	if(isnull(value) || istext(value))
		return value
	if(isnum(value))
		return (isnan(value) || isinf(value)) ? "[value]" : value
	if(ispath(value))
		return list("$path" = "[value]")
	if(islist(value))
		return mcp_encode_list(value, depth)
	if(istype(value, /datum/mcp_record))
		var/datum/mcp_record/record = value
		var/list/pairs = list()
		for(var/i in 1 to length(record.names))
			pairs[++pairs.len] = list(record.names[i], mcp_encode(record.values[i], depth - 1))
		return list("$pairs" = pairs)
	if(value == world)
		return list("$world" = 1)
	if(istype(value, /client))
		var/client/as_client = value
		return list("$client" = as_client.ckey, "ref" = "\ref[as_client]")
	if(istype(value, /datum))
		var/datum/as_datum = value
		var/list/summary = list("$ref" = "\ref[as_datum]", "type" = "[as_datum.type]")
		if(istype(as_datum, /atom))
			var/atom/as_atom = as_datum
			summary["name"] = as_atom.name
			if(as_atom.z)
				summary["xyz"] = "[as_atom.x],[as_atom.y],[as_atom.z]"
			if(ismob(as_atom))
				var/mob/as_mob = as_atom
				if(as_mob.ckey)
					summary["ckey"] = as_mob.ckey
		return summary
	return list("$other" = "[value]")

/proc/mcp_encode_list(list/input, depth)
	var/total = length(input)
	if(depth <= 0)
		return list("$list" = total)
	var/list/keys = list()
	var/list/values = list()
	var/is_assoc = FALSE
	for(var/key in input)
		if(length(keys) >= MCP_LIST_LIMIT)
			break
		var/assoc_value = null
		if(!isnum(key) && !isnull(key))
			try
				assoc_value = input[key]
			catch
				assoc_value = null
		if(!isnull(assoc_value))
			is_assoc = TRUE
		keys[++keys.len] = key
		values[++values.len] = assoc_value
	var/kept = length(keys)
	if(!is_assoc)
		var/list/out = list()
		for(var/i in 1 to kept)
			out[++out.len] = mcp_encode(keys[i], depth - 1)
		if(total > kept)
			out[++out.len] = list("$truncated" = total)
		return out
	// json_encode cannot express a null associated value, so pairs are sent explicitly
	// and byond-mcp folds them back into an object.
	var/list/pairs = list()
	for(var/i in 1 to kept)
		var/key = keys[i]
		var/key_text = istext(key) ? key : ((ispath(key) || isnum(key)) ? "[key]" : "\ref[key]")
		pairs[++pairs.len] = list(key_text, mcp_encode(values[i], depth - 1))
	if(total > kept)
		pairs[++pairs.len] = list("$truncated", total)
	return list("$pairs" = pairs)

/proc/mcp_run_op(list/request)
	if(request["as"])
		var/client/actor = mcp_client(request["as"])
		usr = actor.mob
	switch(request["op"])
		if("status")
			return mcp_status()
		if("clients")
			var/list/out = list()
			for(var/client/connected)
				if(!connected.ckey) // the topic connection itself shows up as a keyless client
					continue
				out[++out.len] = list(
					"ckey" = connected.ckey,
					"key" = connected.key,
					"byond" = "[connected.byond_version].[connected.byond_build]",
					"mob" = connected.mob,
					"ready" = MCP_CLIENT_READY(connected),
				)
			return out
		if("call")
			var/proc_name = request["proc"]
			var/list/call_args = mcp_resolve(request["args"] || list())
			if(isnull(request["target"]))
				var/proc_path = text2path("/proc/[proc_name]")
				if(!proc_path)
					throw EXCEPTION("no global proc named [proc_name]")
				return call(proc_path)(arglist(call_args))
			var/target = mcp_resolve(request["target"])
			if(!hascall(target, proc_name))
				throw EXCEPTION("[target] has no proc or verb named [proc_name]")
			return call(target, proc_name)(arglist(call_args))
		if("get")
			var/target = mcp_resolve(request["target"])
			if(!istype(target, /datum) && !istype(target, /client) && target != world)
				return target
			var/list/names = request["vars"] || target:vars
			var/datum/mcp_record/record = new
			for(var/name in names)
				record.names += name
				record.values[++record.values.len] = target:vars[name]
			return record
		if("set")
			var/target = mcp_resolve(request["target"])
			var/name = request["var"]
			target:vars[name] = mcp_resolve(request["value"])
			return target:vars[name]
		if("new")
			var/new_path = text2path(request["path"])
			if(!ispath(new_path))
				throw EXCEPTION("no such type path [request["path"]]")
			var/list/new_args = mcp_resolve(request["args"] || list())
			if(ispath(new_path, /atom))
				new_args = list(mcp_resolve(request["loc"])) + new_args
			return new new_path(arglist(new_args))
		if("del")
			var/target = mcp_resolve(request["target"])
#ifdef QDEL_HINT_QUEUE
			qdel(target)
#else
			del(target)
#endif
			return 1
		if("find")
			return mcp_find(text2path(request["path"]), request["name"], request["limit"] || 50)
		if("command")
			var/client/receiver = mcp_client(request["ckey"])
			winset(receiver, null, list2params(list("command" = request["command"])))
			return 1
		if("lua")
			return mcp_lua(request["script"])
		if("enter")
			return mcp_enter_game(request)
		if("ui_list")
			return mcp_ui_list(mcp_client(request["ckey"]))
		if("ui_act")
			return mcp_ui_act(mcp_client(request["ckey"]), request)
	throw EXCEPTION("unknown op [request["op"]]")

/// Puts every connected player into a body, side by side, starting the round first where there is one.
/proc/mcp_enter_game(list/request)
	var/body_path
	if(request["path"])
		body_path = text2path(request["path"])
		if(!ispath(body_path, /mob))
			throw EXCEPTION("[request["path"]] is not a mob type")
	var/turf/origin
	if(request["x"])
		origin = locate(request["x"], request["y"], request["z"] || 1)
		if(!origin)
			throw EXCEPTION("no turf at [request["x"]],[request["y"]],[request["z"] || 1]")
#ifdef GAME_STATE_PLAYING
	body_path ||= /mob/living/carbon/human
	if(SSticker.current_state < GAME_STATE_PLAYING)
		SSticker.start_immediately = TRUE
		var/patience = 3000 // deciseconds of world time
		while(SSticker.current_state < GAME_STATE_PLAYING)
			patience -= 5
			if(patience < 0)
				throw EXCEPTION("the round did not start; see the logs")
			sleep(5)
	// Job start landmarks delete themselves at round start; the latejoin spots are what survives.
	if(!origin && length(SSjob.latejoin_trackers))
		origin = get_turf(SSjob.latejoin_trackers[1])
#else
	body_path ||= world.mob
#endif
	origin ||= locate(1, 1, 1)
	var/waited = 0
	while(mcp_client_still_loading())
		waited += 5
		if(waited > 600)
			throw EXCEPTION("a client did not finish logging in; see the logs")
		sleep(5)

	var/list/spots = list(origin)
	for(var/reach in 1 to 2)
		for(var/turf/near in orange(reach, origin))
			if(!near.density)
				spots |= near

	var/list/placed = list()
	var/list/warnings = list()
	for(var/client/player)
		if(!player.ckey)
			continue
		var/turf/spot = spots[(length(placed) % length(spots)) + 1]
		var/mob/body = player.mob
		if(MCP_HAS_BODY(body) && !request["fresh"])
#ifdef GAME_STATE_PLAYING
			body.forceMove(spot)
#else
			body.loc = spot
#endif
			placed += body
			continue
		var/mob/lobby_mob = body
		body = new body_path(spot)
#ifdef GAME_STATE_PLAYING
		try
			if(request["use_prefs"] != 0 && ishuman(body))
				player.prefs.safe_transfer_prefs_to(body)
			if(request["outfit"])
				body:equipOutfit(text2path(request["outfit"]))
		catch(var/thrown)
			warnings += "[player.ckey]: [mcp_describe_exception(thrown)]"
		body.PossessByPlayer(player.ckey)
		if(request["own_character"])
			// What the lobby's own spawn path records, and what codebases check before letting a
			// round write back to the character's save. Late-bound: not every fork has these.
			try
				var/slot = player.prefs:default_slot
				body.mind:original_character_slot_index = slot
				body.mind:set_original_character(body)
				var/list/joined_slots = player:persistent_client:joined_as_slots
				if(!joined_slots)
					joined_slots = list()
					player:persistent_client:joined_as_slots = joined_slots
				joined_slots |= "[slot]"
			catch(var/claim_failure)
				warnings += "[player.ckey]: [mcp_describe_exception(claim_failure)]"
		if(istype(lobby_mob, /mob/dead/new_player))
			qdel(lobby_mob)
#else
		body.key = player.key
		del(lobby_mob)
#endif
		placed += body
	// Bodies nobody controls: more participants than the two windows one machine can open.
	for(var/extra in 1 to (request["bodies"] || 0))
		placed += new body_path(spots[(length(placed) % length(spots)) + 1])
	return list("players" = placed, "warnings" = warnings)

#ifdef UI_INTERACTIVE
/// Everything a player has open that waits for an answer: tgui windows and radial menus.
/proc/mcp_ui_list(client/player)
	usr = player.mob
	var/list/out = list()
	for(var/datum/tgui/ui as anything in player.mob.tgui_open_uis)
		out[++out.len] = list(
			"kind" = "tgui",
			"ui" = ui,
			"title" = ui.title,
			"interface" = ui.interface,
			"object" = ui.src_object,
			"data" = ui.src_object.ui_data(player.mob),
			"static_data" = ui.src_object.ui_static_data(player.mob),
		)
	for(var/id in GLOB.radial_menus)
		var/datum/radial_menu/menu = GLOB.radial_menus[id]
		if(menu.current_user != player)
			continue
		var/list/labels = list()
		for(var/choice_id in menu.choices_values)
			labels += "[menu.choices_values[choice_id]]"
		out[++out.len] = list("kind" = "radial", "ui" = menu, "choices" = labels)
	return out

/// Answers an interface the way the player's click would: same server-side handler, same checks.
/proc/mcp_ui_act(client/player, list/request)
	usr = player.mob
	var/datum/chosen = request["ui"] ? mcp_resolve(request["ui"]) : null
	if(!isnull(request["choice"]))
		var/datum/radial_menu/menu = chosen
		if(!istype(menu))
			for(var/id in GLOB.radial_menus)
				var/datum/radial_menu/candidate = GLOB.radial_menus[id]
				if(candidate.current_user == player)
					menu = candidate
					break
		if(!istype(menu))
			throw EXCEPTION("[player.ckey] has no radial menu open")
		for(var/choice_id in menu.choices_values)
			if("[menu.choices_values[choice_id]]" == request["choice"])
				menu.element_chosen(choice_id, player.mob)
				return 1
		throw EXCEPTION("the radial menu has no choice named [request["choice"]]")

	var/datum/tgui/ui = chosen
	if(!istype(ui))
		var/list/open = player.mob.tgui_open_uis
		if(length(open) != 1)
			throw EXCEPTION("[player.ckey] has [length(open)] tgui windows open; pass `ui` from ui_list")
		ui = open[1]
	ui.process_status()
	if(ui.status != UI_INTERACTIVE)
		throw EXCEPTION("that window is not interactive for [player.ckey] right now")
	ui.on_act_message(request["action"], request["params"] || list(), ui.state)
	return 1
#else
/proc/mcp_ui_list(client/player)
	throw EXCEPTION("this codebase has no tgui")

/proc/mcp_ui_act(client/player, list/request)
	throw EXCEPTION("this codebase has no tgui")
#endif

/proc/mcp_client_still_loading()
	for(var/client/player)
		if(player.ckey && !MCP_CLIENT_READY(player))
			return TRUE
	return FALSE

/proc/mcp_status()
	var/list/out = list(
		"world_time" = world.time,
		"byond" = "[world.byond_version].[world.byond_build]",
		"port" = world.port,
		"cpu" = world.cpu,
	)
	var/list/ckeys = list()
	for(var/client/connected)
		// Only clients that finished logging in count as connected, so callers can act on them at once.
		if(connected.ckey && MCP_CLIENT_READY(connected))
			ckeys += connected.ckey
	out["clients"] = ckeys
#ifdef INITSTAGE_MAX
	// The ticker does not leave "startup" until the world's clock runs, so report init separately.
	out["initialized"] = (Master?.init_stage_completed == INITSTAGE_MAX)
#endif
#ifdef GAME_STATE_PLAYING
	var/static/list/state_names = list("startup", "pregame", "setting_up", "playing", "finished")
	var/state = SSticker?.current_state
	out["game_state"] = isnum(state) ? state_names[state + 1] : "startup"
	out["runtimes"] = GLOB.total_runtimes
	out["log_directory"] = GLOB.log_directory
#endif
	return out

/proc/mcp_find(search_path, name_filter, limit)
	if(!ispath(search_path))
		throw EXCEPTION("no such type path")
	var/list/found = list()
	if(ispath(search_path, /atom))
		for(var/atom/candidate in world)
			if(!istype(candidate, search_path))
				continue
			if(name_filter && !findtext(candidate.name, name_filter))
				continue
			found += candidate
			if(length(found) >= limit)
				break
	else
		for(var/datum/candidate)
			if(!istype(candidate, search_path))
				continue
			found += candidate
			if(length(found) >= limit)
				break
	return found

#if defined(DREAMLUAU_LOAD) && !defined(DISABLE_DREAMLUAU)
/proc/mcp_lua(script)
	var/static/datum/lua_state/state
	if(QDELETED(state))
		state = new("mcp")
		if(QDELETED(state))
			state = null
			throw EXCEPTION("SSlua is not initialized yet")
		SSlua.states += state
	var/log_start = length(state.log)
	var/list/result = state.load_script(script)
	var/list/prints = list()
	for(var/i in (log_start + 1) to length(state.log))
		var/list/entry = state.log[i]
		if(entry["status"] == "print")
			prints += entry["message"]
	return list(
		"status" = result["status"],
		"message" = result["message"],
		"return_values" = result["return_values"],
		"prints" = prints,
	)
#else
/proc/mcp_lua(script)
	throw EXCEPTION("this codebase has no dreamluau support")
#endif

#undef MCP_INLINE_LIMIT
#undef MCP_LIST_LIMIT
